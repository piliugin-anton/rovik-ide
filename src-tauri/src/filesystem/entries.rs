//! Changing the file tree: creating, deleting, renaming, moving and copying
//! entries, and what's checked first.
//!
//! All of these take a stable id (from DirEntryInfo.id / get_path_id), never
//! a raw path, for anything that identifies an *existing* entry. The id is
//! resolved to a real path via `resolve_id` right before touching disk, so a
//! path string the frontend cached earlier (which may differ only in case on
//! case-insensitive filesystems) can never be the thing that actually gets
//! deleted, renamed, or copied. A name joined onto a folder is checked
//! first: `../x` or `a/b` would leave the folder. The work on disk (a
//! folder's copy, a move across drives, the trash) runs off the main thread.

use super::{remember_id, resolve_id, stable_file_id, WatcherState};
use crate::util::blocking;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use tauri::State;

/// Numbers the temporary names of case-only renames.
static CALL_COUNT: AtomicU32 = AtomicU32::new(0);

fn copy_dir_recursive(src: &Path, dest: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let dest_path = dest.join(entry.file_name());
        if file_type.is_symlink() {
            copy_symlink(&entry.path(), &dest_path)?;
        } else if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else {
            fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
}

/// A link is copied as a link: `fs::copy` would copy what it points to (a
/// whole folder's worth for a link to one) and fails on a broken link.
fn copy_symlink(src: &Path, dest: &Path) -> std::io::Result<()> {
    let target = fs::read_link(src)?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, dest)
    }
    #[cfg(windows)]
    {
        // Windows links are typed; a broken one is taken as a file link.
        if fs::metadata(src).is_ok_and(|m| m.is_dir()) {
            std::os::windows::fs::symlink_dir(&target, dest)
        } else {
            std::os::windows::fs::symlink_file(&target, dest)
        }
    }
}

/// Removes `p` for good: a folder with everything in it, or a file.
fn remove_for_good(p: &Path, is_dir: bool) -> std::io::Result<()> {
    if is_dir {
        fs::remove_dir_all(p)
    } else {
        fs::remove_file(p)
    }
}

/// Resolves (and caches) the id for an arbitrary path the frontend already
/// knows about directly — e.g. the project root, which never comes through
/// `read_dir_entries` itself.
#[tauri::command]
pub fn get_path_id(state: State<'_, WatcherState>, path: String) -> Result<String, String> {
    let p = Path::new(&path);
    let id = stable_file_id(p);
    remember_id(&state, &id, p);
    Ok(id)
}

/// Prefixes so the frontend can tell conflicts and bad names apart from
/// unexpected I/O failures (see `isAlreadyExists` / `isInvalidName` in
/// lib/commands.ts — same pattern as the existing `trash-unavailable: `).
const ALREADY_EXISTS_PREFIX: &str = "already-exists: ";
const INVALID_NAME_PREFIX: &str = "invalid-name: ";

/// Device names Windows refuses as a file or folder (stem before any `.`),
/// case-insensitively: they address DOS devices instead of files. Creating
/// or renaming to one fails with a confusing OS error, so reject it here.
const WINDOWS_RESERVED_STEMS: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Validates a single file/folder name (never a path) before it is joined
/// onto a parent directory. Without this, a name like `../x` or `a/b` escapes
/// the directory the id resolved to — through create, rename, move and copy
/// alike.
///
/// `windows_rules` is `cfg!(windows)` in production; a parameter so the
/// Windows-only checks are testable on every OS. The portable checks run
/// everywhere, so a project created on Linux stays openable on Windows.
fn validate_file_name_impl(raw: &str, windows_rules: bool) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(format!("{INVALID_NAME_PREFIX}name must not be empty"));
    }
    if name == "." || name == ".." {
        return Err(format!("{INVALID_NAME_PREFIX}{raw:?} is reserved"));
    }
    if name
        .chars()
        .any(|c| c == '/' || c == '\\' || c == '\0' || c.is_control())
    {
        return Err(format!(
            "{INVALID_NAME_PREFIX}{raw:?}: names can't contain slashes or control characters"
        ));
    }
    // Most filesystems cap a single component at 255 bytes.
    if name.len() > 255 {
        return Err(format!("{INVALID_NAME_PREFIX}{raw:?}: name is too long"));
    }
    if windows_rules {
        if name
            .chars()
            .any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        {
            return Err(format!(
                "{INVALID_NAME_PREFIX}{raw:?}: <>:\"|?* aren't allowed in names on Windows"
            ));
        }
        // Win32 silently strips these, so `foo.` would rename to `foo`.
        if name.ends_with(' ') || name.ends_with('.') {
            return Err(format!(
                "{INVALID_NAME_PREFIX}{raw:?}: names can't end with a space or dot on Windows"
            ));
        }
        let stem = name.split('.').next().unwrap_or(name);
        if WINDOWS_RESERVED_STEMS.contains(&stem.to_ascii_uppercase().as_str()) {
            return Err(format!(
                "{INVALID_NAME_PREFIX}{stem:?} is reserved on Windows (a device name)"
            ));
        }
    }
    Ok(name.to_string())
}

fn validate_file_name(raw: &str) -> Result<String, String> {
    validate_file_name_impl(raw, cfg!(windows))
}

/// The name an entry takes at its destination: `new_name`, checked, or its
/// own. `what`: the action, for the error ("move", "copy").
fn entry_name(from: &Path, new_name: Option<String>, what: &str) -> Result<String, String> {
    match new_name {
        Some(name) => validate_file_name(&name),
        None => from
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .ok_or_else(|| format!("{}: cannot {what} the root folder", from.display())),
    }
}

/// Whether `path` is `ancestor` or lives under it, component-wise.
/// Case-insensitive filesystems (Windows, default macOS) treat `FOO/x` as
/// under `foo`, so compare lowercased there too — otherwise moving a folder
/// into its own differently-cased descendant slips past the guard.
fn is_at_or_under(path: &Path, ancestor: &Path) -> bool {
    if path == ancestor || path.starts_with(ancestor) {
        return true;
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        // One separator too: Windows paths come with `\`, or a mix.
        let lower = |p: &Path| p.to_string_lossy().to_lowercase().replace('\\', "/");
        let (p, a) = (lower(path), lower(ancestor));
        p == a || p.starts_with(&format!("{}/", a.trim_end_matches('/')))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        false
    }
}

/// Whether both paths point at the same file (same inode / file index).
/// This is what tells a case-only rename (`foo` → `FOO`, where the target
/// "exists" but is really the source itself) apart from a genuine conflict.
fn is_same_file(a: &Path, b: &Path) -> bool {
    match (super::file_identity(a), super::file_identity(b)) {
        (Some(x), Some(y)) => x == y,
        // Identity unavailable (broken symlink, exotic fs): same file only
        // when the paths are literally equal; anything else is a conflict.
        _ => a == b,
    }
}

/// The OS error a Win32 call failed with: its HRESULT carries the Win32
/// code (0x8007xxxx), which std maps to a kind (ERROR_ALREADY_EXISTS is
/// AlreadyExists); the HRESULT itself would be an uncategorized error.
#[cfg(windows)]
fn win32_error(e: windows::core::Error) -> std::io::Error {
    let hr = e.code().0 as u32;
    let code = if hr & 0xFFFF_0000 == 0x8007_0000 {
        hr & 0xFFFF
    } else {
        hr
    };
    std::io::Error::from_raw_os_error(code as i32)
}

/// Renames without ever silently replacing an existing destination.
/// `std::fs::rename` maps to `rename(2)` / `MoveFileExW` *with* replace, so
/// a plain check-then-rename has a TOCTOU hole: a file created between the
/// check and the rename gets destroyed. Where the OS offers an atomic
/// no-replace rename (Linux, macOS, Windows), use it; elsewhere fall back to
/// check-then-rename (a tiny residual race there, noted rather than hidden).
fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let c_from = CString::new(from.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "source path contains NUL")
        })?;
        let c_to = CString::new(to.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "target path contains NUL")
        })?;
        // SAFETY: plain C call with valid pointers; no locks held.
        let r = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                c_from.as_ptr(),
                libc::AT_FDCWD,
                c_to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if r == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        // Old kernel/libc without renameat2: best-effort fallback.
        if err.raw_os_error() == Some(libc::ENOSYS) {
            if fs::symlink_metadata(to).is_ok() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "destination exists",
                ));
            }
            return fs::rename(from, to);
        }
        Err(err)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};
        let wide = |p: &Path| {
            p.as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect::<Vec<u16>>()
        };
        let (from_w, to_w) = (wide(from), wide(to));
        // No MOVEFILE_REPLACE_EXISTING: an existing destination fails with
        // ERROR_ALREADY_EXISTS instead of being destroyed. WRITE_THROUGH
        // makes the rename durable before we report success.
        // SAFETY: pointers borrow live Vecs; no locks held across the call.
        return unsafe {
            MoveFileExW(
                PCWSTR::from_raw(from_w.as_ptr()),
                PCWSTR::from_raw(to_w.as_ptr()),
                MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(win32_error);
    }
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let c = |p: &Path| {
            CString::new(p.as_os_str().as_bytes()).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains NUL")
            })
        };
        let (c_from, c_to) = (c(from)?, c(to)?);
        // SAFETY: plain C call with valid pointers; no locks held.
        let r = unsafe { libc::renamex_np(c_from.as_ptr(), c_to.as_ptr(), libc::RENAME_EXCL) };
        if r == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        // A volume without exclusive rename (some network shares): the
        // check-then-rename below.
        if !matches!(err.raw_os_error(), Some(libc::ENOTSUP) | Some(libc::EINVAL)) {
            return Err(err);
        }
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        if fs::symlink_metadata(to).is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "destination exists",
            ));
        }
        fs::rename(from, to)
    }
}

/// `rename_no_replace` plus a short retry on Windows, where a rename can
/// transiently fail with a sharing violation: antivirus, search indexer, or
/// a just-closed reader holding the file for a moment. Retries only
/// permission/sharing failures — anything else (and every platform's
/// AlreadyExists) returns immediately.
fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        // ERROR_SHARING_VIOLATION (32), ERROR_LOCK_VIOLATION (33).
        let transient = |e: &std::io::Error| {
            e.kind() == std::io::ErrorKind::PermissionDenied
                || matches!(e.raw_os_error(), Some(32) | Some(33))
        };
        let mut attempt = 0;
        loop {
            match rename_no_replace(from, to) {
                Ok(()) => return Ok(()),
                Err(e) if transient(&e) && attempt < 5 => {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(50 * attempt as u64));
                }
                Err(e) => return Err(e),
            }
        }
    }
    #[cfg(not(windows))]
    {
        rename_no_replace(from, to)
    }
}

/// Maps a rename/move I/O failure to the message the frontend shows.
fn rename_error(from: &Path, err: std::io::Error) -> String {
    match err.kind() {
        ErrorKind::AlreadyExists => {
            format!("{ALREADY_EXISTS_PREFIX}{}: already exists", from.display())
        }
        ErrorKind::PermissionDenied => {
            format!(
                "{}: permission denied (locked by another app?): {err}",
                from.display()
            )
        }
        _ => format!("{}: {err}", from.display()),
    }
}

/// A new entry's path: the folder `dir_id` names, with `name` checked.
fn new_entry(state: &WatcherState, dir_id: &str, name: &str) -> Result<(PathBuf, String), String> {
    let dir = resolve_id(state, dir_id)?;
    let name = validate_file_name(name)?;
    if !dir.is_dir() {
        return Err(format!("{}: not a directory", dir.display()));
    }
    Ok((dir.join(&name), name))
}

/// Why `name` couldn't be made: taken, or anything else.
fn create_error(name: &str, e: std::io::Error) -> String {
    if e.kind() == ErrorKind::AlreadyExists {
        format!("{ALREADY_EXISTS_PREFIX}{name}: already exists")
    } else {
        format!("{name}: {e}")
    }
}

#[tauri::command]
/// Creates an empty file; returns its path (the tree opens it).
/// Exclusive: fails when the name is taken instead of truncating it —
/// `File::create` would destroy an existing file's contents on a race.
pub fn create_file(
    state: State<'_, WatcherState>,
    dir_id: String,
    name: String,
) -> Result<String, String> {
    let (p, name) = new_entry(&state, &dir_id, &name)?;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&p)
        .map_err(|e| create_error(&name, e))?;
    remember_id(&state, &stable_file_id(&p), &p);
    Ok(p.to_string_lossy().into_owned())
}

#[tauri::command]
/// Creates one directory. Fails when the name is taken (`create_dir_all`
/// would silently succeed on an existing dir and mask the conflict).
pub fn create_dir(
    state: State<'_, WatcherState>,
    dir_id: String,
    name: String,
) -> Result<(), String> {
    let (p, name) = new_entry(&state, &dir_id, &name)?;
    fs::create_dir(&p).map_err(|e| create_error(&name, e))?;
    remember_id(&state, &stable_file_id(&p), &p);
    Ok(())
}

/// Starts the error of a delete that couldn't use the trash, so the frontend
/// can offer to delete permanently instead (lib/commands.ts).
const TRASH_UNAVAILABLE: &str = "trash-unavailable: ";

/// Moves `path` to the system's trash. On macOS through NSFileManager, as
/// other editors do, not the trash crate's default of asking Finder, which
/// may first ask leave to control Finder, plays its sound, and leaves a
/// `.DS_Store` in each folder it touched: Git then lists those as new files.
pub(crate) fn to_trash(path: &Path) -> Result<(), trash::Error> {
    #[cfg(target_os = "macos")]
    let ctx = {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        let mut ctx = trash::TrashContext::default();
        ctx.set_delete_method(DeleteMethod::NsFileManager);
        ctx
    };
    #[cfg(not(target_os = "macos"))]
    let ctx = trash::TrashContext::default();
    ctx.delete(path)
}

/// Deletes a file or folder by moving it to the system trash (Windows'
/// Recycle Bin, the macOS Trash, the freedesktop.org trash on Linux), so it
/// can be restored from there. Where there's no trash (some network drives,
/// or a Linux drive without a trash folder) this fails rather than delete
/// for good; `permanent` then deletes outright, once the user has agreed.
#[tauri::command]
pub async fn delete_path(
    state: State<'_, WatcherState>,
    id: String,
    permanent: Option<bool>,
) -> Result<(), String> {
    let p = resolve_id(&state, &id)?;
    blocking(move || {
        let meta = fs::symlink_metadata(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        if permanent.unwrap_or(false) {
            remove_for_good(&p, meta.is_dir()).map_err(|e| format!("{}: {e}", p.display()))
        } else {
            to_trash(&p).map_err(|e| format!("{TRASH_UNAVAILABLE}{}: {e}", p.display()))
        }
    })
    .await?;
    state.id_to_path.lock().unwrap().remove(&id);
    Ok(())
}

/// Where an entry went: whether it's a new entry there (a copy, whose
/// stable id is its own) rather than the same one renamed.
#[derive(Debug)]
struct Moved {
    to: PathBuf,
    copied: bool,
}

/// Moves `from` to `to`, never over another entry: a `to` that "exists" as
/// the entry itself (a case-only, or normalization-only on macOS, spelling
/// change) goes through a temporary name, as a direct rename may silently do
/// nothing there; one that's another entry is refused. Another drive
/// (`rename` can't cross filesystems): copied, then the source removed.
fn move_entry(from: &Path, to: &Path, name: &str) -> Result<Moved, String> {
    let moved = |copied| Moved {
        to: to.to_path_buf(),
        copied,
    };
    if to == from {
        return Ok(moved(false));
    }
    if fs::symlink_metadata(to).is_ok() {
        if is_same_file(from, to) {
            rename_case_only(from, to)?;
            return Ok(moved(false));
        }
        return Err(format!("{ALREADY_EXISTS_PREFIX}{name}: already exists"));
    }
    match rename_with_retry(from, to) {
        Ok(()) => Ok(moved(false)),
        Err(e) if e.kind() == ErrorKind::CrossesDevices => {
            move_cross_device(from, to)?;
            Ok(moved(true))
        }
        Err(e) => Err(rename_error(from, e)),
    }
}

/// The id of a moved entry follows it. A copy (another drive) has a new
/// inode / file index, so the stable id changes: the old id keeps pointing
/// at the new path (in-flight frontend ids still resolve) and the fresh id
/// is registered too; the tree picks the new ids up on its next
/// `read_dir_entries`.
fn moved_to(state: &WatcherState, id: &str, moved: Moved) -> String {
    remember_id(state, id, &moved.to);
    if moved.copied {
        remember_id(state, &stable_file_id(&moved.to), &moved.to);
    }
    moved.to.to_string_lossy().to_string()
}

/// Renames an entry in place (same parent directory, new file/folder name).
/// The id is unchanged by the rename — same inode/file-index — so we just
/// update where it points. Returns the new absolute path so the frontend can
/// update any open tab.
#[tauri::command]
pub async fn rename_path(
    state: State<'_, WatcherState>,
    id: String,
    new_name: String,
) -> Result<String, String> {
    let from = resolve_id(&state, &id)?;
    let new_name = validate_file_name(&new_name)?;
    let parent = from
        .parent()
        .ok_or_else(|| "cannot rename the root folder".to_string())?;
    let to = parent.join(&new_name);
    let moved = blocking(move || move_entry(&from, &to, &new_name)).await?;
    Ok(moved_to(&state, &id, moved))
}

/// The temp-hop for case-only renames: `from` → unique temp name → `to`.
fn rename_case_only(from: &Path, to: &Path) -> Result<(), String> {
    let parent = from
        .parent()
        .ok_or_else(|| "cannot rename the root folder".to_string())?;
    let tmp = parent.join(format!(
        ".rename-{}-{}.tmp",
        std::process::id(),
        CALL_COUNT.fetch_add(1, Ordering::SeqCst)
    ));
    if fs::symlink_metadata(&tmp).is_ok() {
        return Err(format!("{}: try again", from.display()));
    }
    rename_with_retry(from, &tmp).map_err(|e| rename_error(from, e))?;
    if let Err(e) = rename_with_retry(&tmp, to) {
        // Best effort rollback so the entry doesn't stay at the temp name.
        let _ = fs::rename(&tmp, from);
        return Err(rename_error(from, e));
    }
    Ok(())
}

/// Moves an entry into a different directory (optionally renaming it),
/// i.e. what a cut-then-paste does. `dest_dir_id` must be a directory's id.
/// Returns the new absolute path so the frontend can update any open tab.
/// Never overwrites; moving a folder into itself or a descendant is rejected
/// here (not just in the UI), and a cross-device move falls back to
/// copy-then-delete, since `rename` cannot cross filesystems (EXDEV).
#[tauri::command]
pub async fn move_path(
    state: State<'_, WatcherState>,
    id: String,
    dest_dir_id: String,
    new_name: Option<String>,
) -> Result<String, String> {
    let from = resolve_id(&state, &id)?;
    let dest_dir = resolve_id(&state, &dest_dir_id)?;
    let name = entry_name(&from, new_name, "move")?;
    let moved = blocking(move || {
        if !dest_dir.is_dir() {
            return Err(format!("{}: not a directory", dest_dir.display()));
        }
        if is_at_or_under(&dest_dir, &from) {
            return Err(format!(
                "{INVALID_NAME_PREFIX}can't move {} into itself",
                from.display()
            ));
        }
        move_entry(&from, &dest_dir.join(&name), &name)
    })
    .await?;
    Ok(moved_to(&state, &id, moved))
}

/// EXDEV fallback: copy the entry to the target, verify, then remove the
/// source (a link is refused: its target's bytes aren't it).
fn move_cross_device(from: &Path, to: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(from).map_err(|e| format!("{}: {e}", from.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!(
            "{}: can't move a link across drives (copy it instead)",
            from.display()
        ));
    }
    if meta.is_dir() {
        if let Err(e) = copy_dir_recursive(from, to) {
            // A half copy isn't left behind; the source stays as it was.
            let _ = fs::remove_dir_all(to);
            return Err(format!("{}: {e} (nothing was moved)", from.display()));
        }
    } else {
        fs::copy(from, to).map_err(|e| format!("{}: {e}", from.display()))?;
        // Don't delete the source when the copy looks truncated.
        let dest_len = fs::metadata(to).map(|m| m.len()).unwrap_or(0);
        if meta.len() != dest_len {
            let _ = fs::remove_file(to);
            return Err(format!(
                "{}: copy was incomplete, source kept",
                from.display()
            ));
        }
    }
    remove_for_good(from, meta.is_dir()).map_err(|e| {
        format!(
            "{}: moved, but couldn't remove the source: {e}",
            from.display()
        )
    })
}

/// The target name, or when that's taken the first free "name copy.ext",
/// "name copy 2.ext", "name copy 3.ext", ... (as the macOS Finder names
/// them). Copying a copy counts on from its number instead of adding
/// another " copy".
fn generate_unique_path(dest_dir: &Path, name: &str, is_dir: bool) -> PathBuf {
    let dest = dest_dir.join(name);
    if fs::symlink_metadata(&dest).is_err() {
        return dest;
    }

    // Split into base and extension for files (".gitignore" is all base).
    let (base, ext) = match name.rfind('.') {
        Some(dot) if !is_dir && dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };

    // An existing " copy" / " copy N" suffix: continue from it.
    let (original, mut n) = match base.rfind(" copy") {
        Some(at) => match &base[at + 5..] {
            "" => (&base[..at], 2u64),
            rest => match rest.strip_prefix(' ').and_then(|d| d.parse::<u64>().ok()) {
                Some(k) => (&base[..at], k + 1),
                None => (base, 1),
            },
        },
        None => (base, 1),
    };

    loop {
        let candidate = if n == 1 {
            format!("{original} copy{ext}")
        } else {
            format!("{original} copy {n}{ext}")
        };
        let path = dest_dir.join(candidate);
        if fs::symlink_metadata(&path).is_err() {
            return path;
        }
        n += 1;
    }
}

/// Copies an entry into a different directory (optionally renaming it), i.e.
/// what a copy-then-paste does. `dest_dir_id` must be a directory's id.
/// If a file/folder with the target name already exists, automatically
/// generates a unique name: "name copy.ext", "name copy 2.ext", ...
#[tauri::command]
pub async fn copy_path(
    state: State<'_, WatcherState>,
    id: String,
    dest_dir_id: String,
    new_name: Option<String>,
) -> Result<String, String> {
    let src = resolve_id(&state, &id)?;
    let dest_dir = resolve_id(&state, &dest_dir_id)?;
    let name = entry_name(&src, new_name, "copy")?;
    let dest = blocking(move || copy_entry(&src, &dest_dir, &name)).await?;
    remember_id(&state, &stable_file_id(&dest), &dest);
    // Return the actual path used so frontend can update open tabs if needed
    Ok(dest.to_string_lossy().to_string())
}

/// Copies `src` into `dest_dir` as `name`, or under the first free copy's
/// name when that's taken: the copy's path. A folder copied into itself
/// (or below itself) would copy its own copy, without end: refused.
fn copy_entry(src: &Path, dest_dir: &Path, name: &str) -> Result<PathBuf, String> {
    if !dest_dir.is_dir() {
        return Err(format!("{}: not a directory", dest_dir.display()));
    }
    if is_at_or_under(dest_dir, src) {
        return Err(format!(
            "{INVALID_NAME_PREFIX}can't copy {} into itself",
            src.display()
        ));
    }
    let dest = generate_unique_path(dest_dir, name, src.is_dir());
    let failed = |e: std::io::Error| format!("{}: {e}", src.display());
    let meta = fs::symlink_metadata(src).map_err(failed)?;
    if meta.file_type().is_symlink() {
        copy_symlink(src, &dest).map_err(failed)?;
    } else if meta.is_dir() {
        if let Err(e) = copy_dir_recursive(src, &dest) {
            // A half copy isn't left behind.
            let _ = fs::remove_dir_all(&dest);
            return Err(failed(e));
        }
    } else {
        fs::copy(src, &dest).map_err(failed)?;
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rename-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn names_are_validated_before_touching_disk() {
        // Portable rules, on every OS.
        for bad in ["", "   ", ".", "..", "a/b", "a\\b", "a\0b", "a\x07b"] {
            assert!(validate_file_name_impl(bad, false).is_err(), "{bad:?}");
        }
        assert!(validate_file_name_impl(&"x".repeat(256), false).is_err());
        assert_eq!(
            validate_file_name_impl("  ok.txt  ", false).unwrap(),
            "ok.txt"
        );
        assert!(validate_file_name_impl("don't?.txt", false).is_ok());
        // Windows-only rules, testable anywhere via the flag.
        for bad in ["CON", "con.txt", "LPT1", "foo?", "a<b", "trail. ", "trail."] {
            assert!(validate_file_name_impl(bad, true).is_err(), "{bad:?}");
        }
        assert!(validate_file_name_impl("don't?.txt", true).is_err());
        assert!(validate_file_name_impl("contest.txt", true).is_ok());
        assert!(validate_file_name_impl("okay name (1).txt", true).is_ok());
        // A name given for a move or a copy is checked the same way.
        let from = Path::new("/proj/a.txt");
        assert!(entry_name(from, Some("../x".into()), "copy").is_err());
        assert_eq!(entry_name(from, None, "copy").unwrap(), "a.txt");
        assert!(entry_name(Path::new("/"), None, "move")
            .unwrap_err()
            .contains("root folder"));
    }

    #[test]
    fn descendant_moves_are_detected() {
        let root = Path::new("/proj");
        assert!(is_at_or_under(&root.join("sub/dir"), root));
        assert!(is_at_or_under(root, root));
        assert!(!is_at_or_under(&root.join("other"), &root.join("sub")));
        // A sibling whose name merely shares a prefix is not a descendant.
        assert!(!is_at_or_under(Path::new("/proj2/x"), root));
    }

    #[test]
    fn no_replace_rename_never_overwrites() {
        let dir = scratch("noreplace");
        let (from, to) = (dir.join("a.txt"), dir.join("b.txt"));
        std::fs::write(&from, "new").unwrap();
        std::fs::write(&to, "victim").unwrap();
        let err = rename_no_replace(&from, &to).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "victim");
        assert!(from.exists(), "source is kept on conflict");

        let ok_to = dir.join("c.txt");
        rename_no_replace(&from, &ok_to).unwrap();
        assert_eq!(std::fs::read_to_string(&ok_to).unwrap(), "new");
        assert!(!from.exists());
        // Through `move_entry` too: the entry itself, another, a free name.
        let err = move_entry(&ok_to, &to, "b.txt").unwrap_err();
        assert!(err.starts_with(ALREADY_EXISTS_PREFIX), "{err}");
        assert_eq!(move_entry(&ok_to, &ok_to, "c.txt").unwrap().to, ok_to);
        let moved = move_entry(&ok_to, &dir.join("d.txt"), "d.txt").unwrap();
        assert!(!moved.copied && moved.to.exists() && !ok_to.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn case_only_rename_goes_through_a_temp_name() {
        let dir = scratch("caseonly");
        let from = dir.join("MiXeD.txt");
        std::fs::write(&from, "data").unwrap();
        let to = dir.join("mixed.txt");
        let state = WatcherState::default();
        let id = stable_file_id(&from);
        remember_id(&state, &id, &from);
        // The temp-hop works whether or not the fs is case-sensitive: same
        // helper both platforms use once `is_same_file` matched.
        rename_case_only(&from, &to).unwrap();
        let out = moved_to(
            &state,
            &id,
            Moved {
                to: to.clone(),
                copied: false,
            },
        );
        assert_eq!(Path::new(&out), to);
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "data");
        assert_eq!(resolve_id(&state, &id).unwrap(), to);
        // The folder has the new spelling alone: no old one, no temp file.
        // (Where case doesn't count, `from.exists()` is the new file's.)
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["mixed.txt"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn temp_hop_rename_updates_the_id_mapping() {
        let dir = scratch("temphop");
        let from = dir.join("a.txt");
        std::fs::write(&from, "data").unwrap();
        let to = dir.join("b.txt");
        let state = WatcherState::default();
        let id = stable_file_id(&from);
        remember_id(&state, &id, &from);
        // Direct unit test of the helper both legs use.
        rename_with_retry(&from, &to).unwrap();
        remember_id(&state, &id, &to);
        assert_eq!(resolve_id(&state, &id).unwrap(), to);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cross_device_move_copies_then_removes() {
        let dir = scratch("xdev");
        let (from, to) = (dir.join("orig.txt"), dir.join("moved.txt"));
        std::fs::write(&from, "payload").unwrap();
        let state = WatcherState::default();
        let id = stable_file_id(&from);
        remember_id(&state, &id, &from);
        move_cross_device(&from, &to).unwrap();
        let out = moved_to(
            &state,
            &id,
            Moved {
                to: to.clone(),
                copied: true,
            },
        );
        assert_eq!(Path::new(&out), to);
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "payload");
        assert!(!from.exists(), "source removed after a verified copy");
        // The old id still resolves, and the copy's own id does too.
        assert_eq!(resolve_id(&state, &id).unwrap(), to);
        assert_eq!(resolve_id(&state, &stable_file_id(&to)).unwrap(), to);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn links_refuse_the_cross_device_fallback() {
        let dir = scratch("xdevlink");
        let link = dir.join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("nowhere"), &link).unwrap();
        #[cfg(not(unix))]
        std::fs::write(&link, "x").unwrap();
        // Symlinks must never be "moved" by copying their target's bytes.
        #[cfg(unix)]
        assert!(move_cross_device(&link, &dir.join("out")).is_err());
        assert!(fs::symlink_metadata(&link).is_ok(), "source kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn copies_are_named_like_the_finder_names_them() {
        let dir = std::env::temp_dir().join(format!("unique-path-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let name = |p: PathBuf| p.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            name(generate_unique_path(&dir, "notes.txt", false)),
            "notes.txt"
        );
        fs::write(dir.join("notes.txt"), "").unwrap();
        assert_eq!(
            name(generate_unique_path(&dir, "notes.txt", false)),
            "notes copy.txt"
        );
        fs::write(dir.join("notes copy.txt"), "").unwrap();
        assert_eq!(
            name(generate_unique_path(&dir, "notes.txt", false)),
            "notes copy 2.txt"
        );
        assert_eq!(
            name(generate_unique_path(&dir, "notes copy.txt", false)),
            "notes copy 2.txt"
        );
        fs::write(dir.join("copycat.txt"), "").unwrap();
        assert_eq!(
            name(generate_unique_path(&dir, "copycat.txt", false)),
            "copycat copy.txt"
        );
        fs::write(dir.join(".gitignore"), "").unwrap();
        assert_eq!(
            name(generate_unique_path(&dir, ".gitignore", false)),
            ".gitignore copy"
        );
        fs::create_dir(dir.join("src.d")).unwrap();
        assert_eq!(
            name(generate_unique_path(&dir, "src.d", true)),
            "src.d copy"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_is_never_copied_into_itself() {
        let dir = scratch("copy-self");
        let a = dir.join("a");
        fs::create_dir_all(a.join("sub")).unwrap();
        fs::write(a.join("sub/f.txt"), "f").unwrap();
        // Into itself, or below itself: it would copy its own copy, without end.
        for into in [a.clone(), a.join("sub")] {
            let err = copy_entry(&a, &into, "a").unwrap_err();
            assert!(err.contains("into itself"), "{err}");
        }
        assert!(
            copy_entry(&a, &a.join("sub/f.txt"), "a").is_err(),
            "not a folder"
        );
        // Beside itself, under the first free name.
        let copy = copy_entry(&a, &dir, "a").unwrap();
        assert_eq!(copy, dir.join("a copy"));
        assert_eq!(fs::read_to_string(copy.join("sub/f.txt")).unwrap(), "f");
        assert_eq!(fs::read_to_string(a.join("sub/f.txt")).unwrap(), "f");
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_copy_keeps_its_links_as_links() {
        let dir = std::env::temp_dir().join(format!("copy-links-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let src = dir.join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("sub/a.txt"), "a").unwrap();
        std::os::unix::fs::symlink("sub", src.join("to-sub")).unwrap();
        std::os::unix::fs::symlink("missing", src.join("broken")).unwrap();
        copy_dir_recursive(&src, &dir.join("dest")).unwrap();
        for link in ["to-sub", "broken"] {
            let meta = fs::symlink_metadata(dir.join("dest").join(link)).unwrap();
            assert!(meta.file_type().is_symlink(), "{link}");
        }
        assert_eq!(
            fs::read_link(dir.join("dest/to-sub")).unwrap(),
            Path::new("sub")
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
