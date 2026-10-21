// src-tauri/crates/rovik-core/src/process/appimage.rs
//
// Run from an AppImage, the app's environment isn't the user's. The
// AppImage's launcher (its runtime, linuxdeploy's GTK hook, then AppRun)
// puts the AppImage's folders first in LD_LIBRARY_PATH, PATH, XDG_DATA_DIRS
// and others, points PYTHONHOME, GTK and GStreamer into the AppImage, and
// moves into its `usr` folder. The app needs all of it: its WebKit loads
// the AppImage's libraries and finds its helper processes from that folder
// (`././/lib/…`). The programs it starts don't. A login shell's
// /etc/profile.d runs `flatpak`, which loaded the AppImage's older
// libnghttp2 and stopped with a symbol lookup error, and `python3` doesn't
// start with PYTHONHOME in the AppImage. So the app's own environment stays
// as it is, and each program it starts gets the user's back: the launcher's
// changes, undone here.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The AppImage this process runs from.
pub(super) struct AppImage {
    /// Where its files are (`$APPDIR`, mounted or extracted), and the same
    /// with symlinks resolved, when that's different.
    dirs: Vec<PathBuf>,
    /// Where a program the app starts starts when it isn't told: the folder
    /// the AppImage was started from (`$OWD`), which AppRun left for the
    /// AppImage's `usr` folder; the home folder when that isn't known.
    pub(super) start_dir: Option<PathBuf>,
}

/// The runtime's own variables, which only mean something to this process.
/// `ARGV0` does harm, too: zsh starts every command under that name.
const RUNTIME_OWN: [&str; 4] = ["APPIMAGE", "APPDIR", "ARGV0", "OWD"];

/// The AppImage this process runs from, if it does: `$APPDIR` is set and
/// this executable is inside it. An `$APPDIR` inherited from another
/// AppImage's process doesn't count.
pub(super) fn current() -> Option<&'static AppImage> {
    static CURRENT: OnceLock<Option<AppImage>> = OnceLock::new();
    CURRENT
        .get_or_init(|| {
            let dir = PathBuf::from(std::env::var_os("APPDIR")?);
            let real = std::fs::canonicalize(&dir).ok()?;
            let exe = std::env::current_exe().ok()?;
            if !dir.is_absolute() || !exe.starts_with(&real) {
                return None;
            }
            let mut dirs = vec![dir];
            if !dirs.contains(&real) {
                dirs.push(real);
            }
            let in_image =
                std::env::current_dir().is_ok_and(|cwd| dirs.iter().any(|d| cwd.starts_with(d)));
            let start_dir = in_image
                .then(|| {
                    ["OWD", "HOME"]
                        .iter()
                        .filter_map(std::env::var_os)
                        .map(PathBuf::from)
                        .find(|d| d.is_absolute() && d.is_dir())
                })
                .flatten();
            Some(AppImage { dirs, start_dir })
        })
        .as_ref()
}

impl AppImage {
    /// Whether `entry`, a path or one of a list's, is in the AppImage.
    fn has(&self, entry: &[u8]) -> bool {
        let path = Path::new(OsStr::from_bytes(entry));
        path.is_absolute() && self.dirs.iter().any(|d| path.starts_with(d))
    }

    /// The changes that give a program the app starts the environment the
    /// user had, `vars` being this process's: each variable to set, or to
    /// remove (None). Lists lose the AppImage's entries and keep the
    /// user's, in their order; a variable with nothing of the user's left
    /// is removed. A value the launcher replaced can't be had back, and is
    /// removed too: it was the AppImage's.
    pub(super) fn undo(&self, vars: &[(OsString, OsString)]) -> Vec<(OsString, Option<OsString>)> {
        let points_in = |key: &str| vars.iter().any(|(k, v)| k == key && self.has(v.as_bytes()));
        // linuxdeploy's GTK hook ran (it points GTK_DATA_PREFIX at the
        // AppImage), and AppRun (PYTHONHOME).
        let gtk_hook = points_in("GTK_DATA_PREFIX");
        let apprun = points_in("PYTHONHOME");

        let mut changes = Vec::new();
        for (key, value) in vars {
            let name = key.to_str().unwrap_or_default();
            let launchers = RUNTIME_OWN.contains(&name)
                // Adwaita, as the AppImage's GTK can't draw the system's
                // themes; the system's GTK can.
                || (gtk_hook && name == "GTK_THEME")
                // Set with PYTHONHOME, so that Python doesn't write into the
                // AppImage.
                || (apprun && name == "PYTHONDONTWRITEBYTECODE");
            if launchers {
                changes.push((key.clone(), None));
                continue;
            }
            let entries: Vec<&[u8]> = value.as_bytes().split(|b| *b == b':').collect();
            if !entries.iter().any(|e| self.has(e)) {
                continue;
            }
            let mut kept: Vec<&[u8]> = Vec::new();
            let mut after_own = false;
            for entry in entries {
                if self.has(entry) {
                    after_own = true;
                    continue;
                }
                // The hook's `$APPDIR/usr/share:/usr/share:$XDG_DATA_DIRS`:
                // that /usr/share is the hook's, not the user's.
                let hooks = gtk_hook
                    && after_own
                    && name == "XDG_DATA_DIRS"
                    && Path::new(OsStr::from_bytes(entry)) == Path::new("/usr/share");
                after_own = false;
                if !hooks {
                    kept.push(entry);
                }
            }
            let value =
                (!kept.iter().all(|e| e.is_empty())).then(|| OsString::from_vec(kept.join(&b':')));
            changes.push((key.clone(), value));
        }
        changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const DIR: &str = "/tmp/.mount_RovikAbC123";

    fn image() -> AppImage {
        AppImage {
            dirs: vec![PathBuf::from(DIR)],
            start_dir: None,
        }
    }

    type Env = BTreeMap<String, String>;

    fn env(pairs: &[(&str, &str)]) -> Env {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// `original` as the app gets it, launched from an AppImage at `dir`
    /// as Tauri bundles it: the runtime, linuxdeploy's GTK hook (sourced
    /// first by the AppRun script), then the classic AppRun binary. Their
    /// own code, line for line, in the order it runs.
    fn launch(original: &Env, dir: &str) -> Env {
        let mut e = original.clone();
        let get = |e: &Env, k: &str| e.get(k).cloned();
        // The runtime.
        e.insert("APPIMAGE".into(), "/home/u/Apps/Rovik.AppImage".into());
        e.insert("APPDIR".into(), dir.into());
        e.insert("OWD".into(), "/home/u/Downloads".into());
        e.insert("ARGV0".into(), "./Rovik.AppImage".into());
        // linuxdeploy-plugin-gtk.sh, Tauri's.
        let xdg = get(&e, "XDG_DATA_DIRS").unwrap_or_default();
        e.insert("GTK_DATA_PREFIX".into(), dir.into());
        e.insert("GTK_THEME".into(), "Adwaita:light".into());
        e.insert(
            "XDG_DATA_DIRS".into(),
            format!("{dir}/usr/share:/usr/share:{xdg}"),
        );
        e.insert(
            "GSETTINGS_SCHEMA_DIR".into(),
            format!("{dir}//usr/share/glib-2.0/schemas"),
        );
        e.insert(
            "GI_TYPELIB_PATH".into(),
            format!("{dir}//usr/lib/girepository-1.0"),
        );
        e.insert(
            "GIO_MODULE_DIR".into(),
            format!("{dir}//usr/lib/gio/modules"),
        );
        e.insert("GTK_EXE_PREFIX".into(), format!("{dir}//usr"));
        e.insert("GTK_PATH".into(), format!("{dir}//usr/lib/gtk-3.0"));
        e.insert(
            "GTK_IM_MODULE_FILE".into(),
            format!("{dir}//usr/lib/gtk-3.0/3.0.0/immodules.cache"),
        );
        e.insert(
            "GDK_PIXBUF_MODULE_FILE".into(),
            format!("{dir}//usr/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache"),
        );
        // AppRun (AppImageKit's classic one): each prepended to the value it
        // finds, or to "" (XDG_DATA_DIRS: to the spec's default).
        let a = dir;
        let old = |e: &Env, k: &str| get(e, k).unwrap_or_default();
        e.insert("PYTHONHOME".into(), format!("{a}/usr/"));
        let v = format!(
            "{a}/usr/bin/:{a}/usr/sbin/:{a}/usr/games/:{a}/bin/:{a}/sbin/:{}",
            old(&e, "PATH")
        );
        e.insert("PATH".into(), v);
        let v = format!(
            "{a}/usr/lib/:{a}/usr/lib/i386-linux-gnu/:{a}/usr/lib/x86_64-linux-gnu/:{a}/usr/lib32/:{a}/usr/lib64/:{a}/lib/:{a}/lib/i386-linux-gnu/:{a}/lib/x86_64-linux-gnu/:{a}/lib32/:{a}/lib64/:{}",
            old(&e, "LD_LIBRARY_PATH")
        );
        e.insert("LD_LIBRARY_PATH".into(), v);
        let v = format!("{a}/usr/share/pyshared/:{}", old(&e, "PYTHONPATH"));
        e.insert("PYTHONPATH".into(), v);
        let xdg =
            get(&e, "XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share/:/usr/share/".into());
        e.insert("XDG_DATA_DIRS".into(), format!("{a}/usr/share/:{xdg}"));
        let v = format!(
            "{a}/usr/share/perl5/:{a}/usr/lib/perl5/:{}",
            old(&e, "PERLLIB")
        );
        e.insert("PERLLIB".into(), v);
        let v = format!(
            "{a}/usr/share/glib-2.0/schemas/:{}",
            old(&e, "GSETTINGS_SCHEMA_DIR")
        );
        e.insert("GSETTINGS_SCHEMA_DIR".into(), v);
        let v = format!(
            "{a}/usr/lib/qt4/plugins/:{a}/usr/lib/qt5/plugins/:{}",
            old(&e, "QT_PLUGIN_PATH")
        );
        e.insert("QT_PLUGIN_PATH".into(), v);
        let v = format!(
            "{a}/usr/lib/gstreamer:{}",
            old(&e, "GST_PLUGIN_SYSTEM_PATH")
        );
        e.insert("GST_PLUGIN_SYSTEM_PATH".into(), v);
        let v = format!(
            "{a}/usr/lib/gstreamer-1.0:{}",
            old(&e, "GST_PLUGIN_SYSTEM_PATH_1_0")
        );
        e.insert("GST_PLUGIN_SYSTEM_PATH_1_0".into(), v);
        e.insert("PYTHONDONTWRITEBYTECODE".into(), "1".into());
        // bash, running AppRun.wrapped.
        e.insert("_".into(), format!("{dir}/AppRun.wrapped"));
        e
    }

    /// `launched` with `image`'s changes made: what a program the app starts
    /// gets.
    fn child(image: &AppImage, launched: &Env) -> Env {
        let vars: Vec<(OsString, OsString)> =
            launched.iter().map(|(k, v)| (k.into(), v.into())).collect();
        let mut out = launched.clone();
        for (key, value) in image.undo(&vars) {
            let key = key.into_string().unwrap();
            match value {
                Some(v) => out.insert(key, v.into_string().unwrap()),
                None => out.remove(&key),
            };
        }
        out
    }

    #[test]
    fn a_program_the_app_starts_gets_the_users_environment_back() {
        let original = env(&[
            ("HOME", "/home/u"),
            ("SHELL", "/bin/zsh"),
            (
                "PATH",
                "/home/u/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/home/u/.cargo/bin",
            ),
            ("LD_LIBRARY_PATH", "/opt/cuda/lib64"),
            ("PYTHONPATH", "/home/u/lib/python"),
            (
                "XDG_DATA_DIRS",
                "/usr/share/gnome:/usr/local/share/:/usr/share/:/var/lib/snapd/desktop",
            ),
            ("LANG", "en_US.UTF-8"),
        ]);
        assert_eq!(child(&image(), &launch(&original, DIR)), original);
    }

    #[test]
    fn what_the_user_hadnt_set_stays_unset() {
        // A desktop session's minimum: no XDG_DATA_DIRS (the spec's default
        // applies), no LD_LIBRARY_PATH; an empty entry there would have been
        // the current folder.
        let original = env(&[("HOME", "/home/u"), ("PATH", "/usr/bin:/bin")]);
        assert_eq!(child(&image(), &launch(&original, DIR)), original);
    }

    #[test]
    fn the_users_own_usr_share_is_kept() {
        let original = env(&[
            ("PATH", "/usr/bin"),
            ("XDG_DATA_DIRS", "/usr/share:/opt/share"),
        ]);
        assert_eq!(child(&image(), &launch(&original, DIR)), original);
        // Without the GTK hook, a /usr/share after the AppImage's folder is
        // the user's.
        let launched = env(&[(
            "XDG_DATA_DIRS",
            &format!("{DIR}/usr/share/:/usr/share:/opt/share"),
        )]);
        assert_eq!(
            child(&image(), &launched),
            env(&[("XDG_DATA_DIRS", "/usr/share:/opt/share")])
        );
    }

    #[test]
    fn only_what_is_in_the_appimage_goes() {
        // A folder whose name starts like the AppImage's isn't in it, and a
        // relative entry is no one's to judge; the user's PYTHONDONTWRITEBYTECODE
        // is theirs when AppRun didn't set PYTHONHOME.
        let launched = env(&[
            ("PATH", &format!("{DIR}/usr/bin:{DIR}x/bin:bin:/usr/bin")),
            ("PYTHONDONTWRITEBYTECODE", "1"),
            ("GTK_THEME", "Yaru-dark"),
            ("EDITOR", "vim"),
        ]);
        assert_eq!(
            child(&image(), &launched),
            env(&[
                ("PATH", &format!("{DIR}x/bin:bin:/usr/bin")),
                ("PYTHONDONTWRITEBYTECODE", "1"),
                ("GTK_THEME", "Yaru-dark"),
                ("EDITOR", "vim"),
            ])
        );
    }

    /// The app started again by itself (an update's restart, "New window"),
    /// at another mount: started with the user's environment, as a program
    /// the app starts gets it, the new one's programs get the user's back.
    /// Started with the app's own, as Tauri's restart did, they kept the old
    /// AppImage's folders, its libraries first: a login shell's flatpak
    /// loaded them, while the old mount stayed, and stopped.
    #[test]
    fn the_app_started_again_by_itself_gives_its_programs_the_users_environment() {
        const NEW: &str = "/tmp/.mount_RovikXyZ789";
        let original = env(&[
            ("HOME", "/home/u"),
            ("PATH", "/usr/bin:/bin"),
            ("LD_LIBRARY_PATH", "/opt/cuda/lib64"),
        ]);
        let new = AppImage {
            dirs: vec![PathBuf::from(NEW)],
            start_dir: None,
        };
        let first = launch(&original, DIR);
        let again = launch(&child(&image(), &first), NEW);
        assert_eq!(child(&new, &again), original);

        let raw = child(&new, &launch(&first, NEW));
        assert!(
            raw["LD_LIBRARY_PATH"].starts_with(&format!("{DIR}/usr/lib/")),
            "{raw:?}"
        );
    }

    #[test]
    fn the_appimage_is_also_found_where_symlinks_lead() {
        let image = AppImage {
            dirs: vec![
                PathBuf::from("/tmp/.mount_x"),
                PathBuf::from("/private/tmp/.mount_x"),
            ],
            start_dir: None,
        };
        let launched = env(&[(
            "LD_LIBRARY_PATH",
            "/tmp/.mount_x/usr/lib:/private/tmp/.mount_x/usr/lib:/opt/lib",
        )]);
        assert_eq!(
            child(&image, &launched),
            env(&[("LD_LIBRARY_PATH", "/opt/lib")])
        );
    }
}
