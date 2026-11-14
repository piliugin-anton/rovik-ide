//! Qualify the controller's designated, host-delegated cgroup parent.
//!
//! A parent from model text or an environment variable could escape the daemon's
//! aggregate limits. The kernel's current membership is the only discovery input.

#[cfg(target_os = "linux")]
use crate::require;
use crate::{Error, Result};
#[cfg(target_os = "linux")]
use std::path::Path;

#[cfg(target_os = "linux")]
const SUBGROUP: &str = "rovik-controller";
const HELP: &str = "Rootless Podman verification requires a delegated cgroup v2 service with Delegate=cpu memory pids and DelegateSubgroup=rovik-controller (systemd 254 or newer). Restart the updated Rovik service, or select a working local Docker engine.";

#[derive(Debug, Clone)]
pub(super) struct Parent(String);

impl Parent {
    pub(super) fn discover() -> Result<Self> {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt;
            let uid = std::fs::metadata("/proc/self")?.uid();
            require(uid != 0, "Podman verification must run as a non-root user")?;
            Self::read(
                Path::new("/proc/self/cgroup"),
                Path::new("/sys/fs/cgroup"),
                uid,
            )
            .map_err(|error| Error::new(format!("{HELP} ({error})")))
        }
        #[cfg(not(target_os = "linux"))]
        Err(Error::new(HELP.into()))
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }

    #[cfg(target_os = "linux")]
    fn read(membership: &Path, root: &Path, uid: u32) -> Result<Self> {
        use std::{
            io::Write,
            os::unix::fs::{MetadataExt, OpenOptionsExt},
        };
        Self::read_with(membership, root, uid, |path, controllers| {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(nix::libc::O_NOFOLLOW)
                .open(path)?;
            require(
                file.metadata()?.uid() == uid,
                "cgroup control ownership changed",
            )?;
            file.write_all(controllers.as_bytes())?;
            Ok(())
        })
    }

    #[cfg(target_os = "linux")]
    fn read_with(
        membership: &Path,
        root: &Path,
        uid: u32,
        mut enable: impl FnMut(&Path, &str) -> Result<()>,
    ) -> Result<Self> {
        use std::{fs, os::unix::fs::MetadataExt};
        let membership = bounded_text(membership)?;
        let mut unified = membership
            .lines()
            .filter_map(|line| line.strip_prefix("0::"));
        let current = unified
            .next()
            .ok_or_else(|| Error::new("no unified cgroup membership".into()))?;
        require(
            unified.next().is_none(),
            "ambiguous unified cgroup membership",
        )?;
        let parent = parent_path(current)?;
        require(
            fs::symlink_metadata(root)?.is_dir(),
            "cgroup root is not a directory",
        )?;
        let mut directory = root.to_path_buf();
        for part in current.trim_start_matches('/').split('/') {
            directory.push(part);
            let metadata = fs::symlink_metadata(&directory)?;
            require(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "cgroup path is not a real directory",
            )?;
        }
        require(
            fs::metadata(&directory)?.uid() == uid,
            "controller subgroup is not owned by this user",
        )?;
        let directory = directory
            .parent()
            .ok_or_else(|| Error::new("missing delegated parent".into()))?;
        require(
            fs::metadata(directory)?.uid() == uid,
            "cgroup parent is not delegated to this user",
        )?;
        for name in ["cgroup.procs", "cgroup.subtree_control"] {
            let metadata = fs::symlink_metadata(directory.join(name))?;
            require(
                metadata.is_file() && !metadata.file_type().is_symlink() && metadata.uid() == uid,
                "cgroup parent control files are not delegated to this user",
            )?;
        }
        require(
            bounded_text(&directory.join("cgroup.procs"))?
                .trim()
                .is_empty(),
            "delegated parent still contains controller processes",
        )?;
        require(
            bounded_text(&directory.join("cgroup.type"))?.trim() == "domain",
            "delegated cgroup is not a domain",
        )?;
        require(
            required_controllers(&bounded_text(&directory.join("cgroup.controllers"))?),
            "cgroup parent lacks delegated cpu, memory or pids controllers",
        )?;
        let subtree = directory.join("cgroup.subtree_control");
        if !required_controllers(&bounded_text(&subtree)?) {
            // systemd makes controllers available to the delegated unit; the
            // delegate enables them for its own children. This only adds fixed
            // controllers to the empty, owned parent and changes no limits.
            enable(&subtree, "+cpu +memory +pids")?;
        }
        require(
            required_controllers(&bounded_text(&subtree)?),
            "delegated cgroup controllers could not be enabled",
        )?;
        Ok(Self(parent.into()))
    }
}

#[cfg(target_os = "linux")]
fn required_controllers(text: &str) -> bool {
    ["cpu", "memory", "pids"]
        .iter()
        .all(|required| text.split_whitespace().any(|value| value == *required))
}

#[cfg(target_os = "linux")]
fn parent_path(current: &str) -> Result<&str> {
    require(
        current.starts_with('/')
            && !current.chars().any(char::is_control)
            && current[1..]
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".."),
        "invalid controller cgroup path",
    )?;
    let (parent, subgroup) = current
        .rsplit_once('/')
        .ok_or_else(|| Error::new("missing controller subgroup".into()))?;
    require(
        subgroup == SUBGROUP && !parent.is_empty(),
        "controller is outside its designated delegated subgroup",
    )?;
    Ok(parent)
}

#[cfg(target_os = "linux")]
fn bounded_text(path: &Path) -> Result<String> {
    use std::io::Read;
    let metadata = std::fs::symlink_metadata(path)?;
    require(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "cgroup metadata is not a regular file",
    )?;
    let mut text = String::new();
    std::fs::File::open(path)?
        .take(65_537)
        .read_to_string(&mut text)?;
    require(text.len() <= 65_536, "cgroup metadata exceeded its bound")?;
    Ok(text)
}

#[cfg(all(test, target_os = "linux"))]
pub(super) mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{symlink, MetadataExt},
    };

    pub(in crate::runner) fn fixture(root: &Path) -> Parent {
        let parent = root.join("cgroups/service");
        fs::create_dir_all(parent.join(SUBGROUP)).unwrap();
        fs::write(root.join("membership"), "0::/service/rovik-controller\n").unwrap();
        for (name, value) in [
            ("cgroup.procs", ""),
            ("cgroup.type", "domain\n"),
            ("cgroup.controllers", "cpu memory pids\n"),
            ("cgroup.subtree_control", "cpu memory pids\n"),
        ] {
            fs::write(parent.join(name), value).unwrap();
        }
        read_fixture(root).unwrap()
    }

    fn read_fixture(root: &Path) -> Result<Parent> {
        let uid = fs::metadata(root)?.uid();
        Parent::read(&root.join("membership"), &root.join("cgroups"), uid)
    }

    #[test]
    fn only_the_designated_nonroot_parent_can_be_selected() {
        assert_eq!(
            parent_path("/rovik.slice/agent.service/rovik-controller").unwrap(),
            "/rovik.slice/agent.service"
        );
        for invalid in [
            "/",
            "/rovik-controller",
            "relative/rovik-controller",
            "/service",
            "/service/runtime",
            "/service/../rovik-controller",
            "/service//rovik-controller",
            "",
            "/service/rovik-controller\n",
        ] {
            assert!(parent_path(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn delegation_requires_ownership_empty_parent_and_all_enabled_controllers() {
        let root = tempfile::tempdir().unwrap();
        let parent = fixture(root.path());
        assert_eq!(parent.as_str(), "/service");
        for (name, invalid) in [
            ("cgroup.procs", "42\n"),
            ("cgroup.type", "threaded"),
            ("cgroup.controllers", "memory pids"),
        ] {
            let path = root.path().join("cgroups/service").join(name);
            let original = fs::read(&path).unwrap();
            fs::write(&path, invalid).unwrap();
            assert!(read_fixture(root.path()).is_err(), "{name}");
            fs::write(path, original).unwrap();
        }
        let uid = fs::metadata(root.path()).unwrap().uid();
        assert!(Parent::read(
            &root.path().join("membership"),
            &root.path().join("cgroups"),
            uid + 1
        )
        .is_err());
        let path = root.path().join("cgroups/service/cgroup.subtree_control");
        fs::rename(&path, path.with_extension("saved")).unwrap();
        symlink(path.with_extension("saved"), path).unwrap();
        assert!(read_fixture(root.path()).is_err());
    }

    #[test]
    fn only_missing_fixed_controllers_are_enabled_on_the_validated_parent() {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path());
        let membership = root.path().join("membership");
        let cgroups = root.path().join("cgroups");
        let subtree = cgroups.join("service/cgroup.subtree_control");
        let uid = fs::metadata(root.path()).unwrap().uid();
        fs::write(&subtree, "io pids\n").unwrap();
        let mut writes = 0;
        let parent = Parent::read_with(&membership, &cgroups, uid, |path, value| {
            assert_eq!(path, subtree);
            assert_eq!(value, "+cpu +memory +pids");
            writes += 1;
            // Emulate cgroupfs, which preserves previously enabled controllers.
            fs::write(path, "cpu io memory pids\n")?;
            Ok(())
        })
        .unwrap();
        assert_eq!(writes, 1);
        assert_eq!(parent.as_str(), "/service");
        Parent::read_with(&membership, &cgroups, uid, |_, _| panic!("already enabled")).unwrap();
        fs::write(&subtree, "").unwrap();
        assert!(
            Parent::read_with(&membership, &cgroups, uid, |_, _| Ok(())).is_err(),
            "an ignored enable must fail closed"
        );
        fs::write(cgroups.join("service/cgroup.controllers"), "memory pids").unwrap();
        assert!(Parent::read_with(&membership, &cgroups, uid, |_, _| panic!(
            "unavailable controller"
        ))
        .is_err());
    }

    #[test]
    fn membership_and_cgroup_directories_cannot_redirect_parent_discovery() {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path());
        let membership = root.path().join("membership");
        for invalid in [
            "0::/service/rovik-controller\n0::/other/rovik-controller\n",
            "1:memory:/service/rovik-controller\n",
        ] {
            fs::write(&membership, invalid).unwrap();
            assert!(read_fixture(root.path()).is_err());
        }
        fs::write(&membership, "0::/service/rovik-controller\n").unwrap();
        let group = root.path().join("cgroups/service");
        fs::rename(&group, group.with_extension("saved")).unwrap();
        symlink(group.with_extension("saved"), group).unwrap();
        assert!(read_fixture(root.path()).is_err());
    }
}
