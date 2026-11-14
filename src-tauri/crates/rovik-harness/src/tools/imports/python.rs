//! Resolve Python modules from the prepared search path without importing packages.
use super::{scan::Import, Target};
use crate::{require, Error, Result};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Environment {
    paths: Vec<PathBuf>,
    builtins: BTreeSet<String>,
    standard: BTreeSet<String>,
}

struct Resolver {
    project: PathBuf,
    roots: Vec<PathBuf>,
    builtins: BTreeSet<String>,
}
impl Resolver {
    fn new(project: &Path, roots: Vec<PathBuf>, builtins: BTreeSet<String>) -> Result<Self> {
        let project = project.canonicalize()?;
        require(project.is_dir(), "Python project is not a directory")?;
        let mut existing = vec![project.clone()];
        for root in roots {
            if root.is_dir() {
                existing.push(root.canonicalize()?);
            } else {
                require(
                    !root.is_file(),
                    "Python zip import roots require a separate architecture checker",
                )?;
            }
        }
        Ok(Self {
            project,
            roots: existing,
            builtins,
        })
    }
    fn target(&self, path: &Path, name: &str) -> Result<Target> {
        let path = path.canonicalize()?;
        if let Ok(relative) = path.strip_prefix(&self.project) {
            return Ok(Target {
                id: relative
                    .iter()
                    .map(|part| {
                        part.to_str()
                            .ok_or_else(|| Error::new("invalid Python module path".into()))
                    })
                    .collect::<Result<Vec<_>>>()?
                    .join("/"),
                external: false,
            });
        }
        require(
            self.roots.iter().any(|root| path.starts_with(root)),
            "Python import escaped its prepared search roots",
        )?;
        Ok(Target {
            id: format!("python:{name}"),
            external: true,
        })
    }
    fn module_file(directory: &Path, part: &str) -> Result<Option<PathBuf>> {
        let file = directory.join(format!("{part}.py"));
        if file.is_file() {
            return Ok(Some(file));
        }
        if directory.is_dir() {
            for entry in std::fs::read_dir(directory)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with(&format!("{part}."))
                    && name.ends_with(".so")
                    && entry.file_type()?.is_file()
                {
                    return Ok(Some(entry.path()));
                }
            }
        }
        Ok(None)
    }
    fn lookup(&self, module: &str) -> Result<Option<Vec<Target>>> {
        let parts: Vec<_> = module.split('.').collect();
        require(
            parts.iter().all(|part| {
                !part.is_empty() && part.chars().all(|c| c == '_' || c.is_alphanumeric())
            }),
            "invalid Python module name",
        )?;
        if self.builtins.contains(parts[0]) {
            return Ok((parts.len() == 1).then(|| {
                vec![Target {
                    id: format!("python:{module}"),
                    external: true,
                }]
            }));
        }
        let mut search = self.roots.clone();
        let mut targets = Vec::new();
        for (index, part) in parts.iter().enumerate() {
            let name = parts[..=index].join(".");
            let mut namespace = Vec::new();
            let mut found = None;
            for root in &search {
                let directory = root.join(part);
                if let Some(init) = Self::module_file(&directory, "__init__")? {
                    found = Some((init, Some(directory)));
                    break;
                }
                if let Some(file) = Self::module_file(root, part)? {
                    found = Some((file, None));
                    break;
                }
                if directory.is_dir() {
                    namespace.push(directory);
                }
            }
            if let Some((file, directory)) = found {
                targets.push(self.target(&file, &name)?);
                if index + 1 < parts.len() {
                    let Some(directory) = directory else {
                        return Ok(None);
                    };
                    search = vec![directory];
                }
            } else if !namespace.is_empty() {
                for path in &namespace {
                    targets.push(self.target(path, &name)?);
                }
                search = namespace;
            } else {
                return Ok(None);
            }
        }
        Ok(Some(targets))
    }
    fn resolve(&self, import: &Import) -> Result<Vec<Target>> {
        let level = import.module.chars().take_while(|c| *c == '.').count();
        let module = if level == 0 {
            import.module.clone()
        } else {
            let parent = Path::new(&import.source)
                .parent()
                .ok_or_else(|| Error::new("relative import lacks a package".into()))?;
            let mut parts: Vec<_> = parent
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect();
            require(
                level <= parts.len(),
                "relative Python import escapes its package",
            )?;
            parts.truncate(parts.len() - level + 1);
            let suffix = &import.module[level..];
            if !suffix.is_empty() {
                parts.push(suffix.into());
            }
            parts.join(".")
        };
        let mut targets = self.lookup(&module)?.ok_or_else(|| {
            Error::new(format!(
                "unresolved Python import {module} in {}",
                import.source
            ))
        })?;
        if let Some(member) = &import.member {
            // Importing a value still loads the base module. A discoverable submodule
            // also contributes its own edges, including every package initializer.
            if let Some(submodule) = self.lookup(&format!("{module}.{member}"))? {
                targets.extend(submodule);
            }
        }
        Ok(targets)
    }
}

pub(super) fn resolve(
    files: &BTreeMap<String, String>,
    imports: &[Import],
) -> Result<Vec<Vec<Target>>> {
    use super::super::{args, checked, runtime};
    let bytes = checked(&args(&[runtime::PYTHON, "-I", "-c", "import json,sys; print(json.dumps({'paths':sys.path,'builtins':list(sys.builtin_module_names),'standard':list(sys.stdlib_module_names)}))"]), Path::new("/build"))?;
    let environment: Environment = crate::wire::decode(bytes.as_bytes())?;
    // Frozen/preloaded standard modules may win before pytest extends sys.path.
    // Refuse that ambiguity rather than claiming a project shadow is authoritative.
    for path in files.keys() {
        let top = path
            .split('/')
            .next()
            .unwrap_or(path)
            .trim_end_matches(".py");
        require(
            !environment.standard.contains(top),
            format!("project shadows Python standard module {top}"),
        )?;
    }
    let mut roots = vec![PathBuf::from("/build/site")];
    roots.extend(environment.paths);
    let resolver = Resolver::new(Path::new(runtime::PROJECT), roots, environment.builtins)?;
    imports
        .iter()
        .map(|import| resolver.resolve(import))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn relative_imports_namespace_packages_and_initializers_are_all_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        for path in [
            "pkg/__init__.py",
            "pkg/domain.py",
            "pkg/helpers/__init__.py",
            "pkg/helpers/api.py",
            "namespace/infra.py",
        ] {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let resolver = Resolver::new(dir.path(), vec![], ["sys".into()].into()).unwrap();
        let import = Import {
            source: "pkg/domain.py".into(),
            module: ".helpers".into(),
            member: Some("api".into()),
            require: false,
            runtime_source: None,
        };
        let targets = resolver.resolve(&import).unwrap();
        for expected in [
            "pkg/__init__.py",
            "pkg/helpers/__init__.py",
            "pkg/helpers/api.py",
        ] {
            assert!(targets.iter().any(|target| target.id == expected));
        }
        assert!(resolver
            .lookup("namespace.infra")
            .unwrap()
            .unwrap()
            .iter()
            .any(|target| target.id == "namespace/infra.py"));
        assert!(resolver.lookup("pkg.domain.nonexistent").unwrap().is_none());
        assert!(resolver
            .resolve(&Import {
                module: "..escape".into(),
                ..import
            })
            .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn canonical_search_roots_do_not_allow_imports_to_escape_through_symlinks() {
        let project = tempfile::tempdir().unwrap();
        let dependencies = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(dependencies.path().join("allowed.py"), "").unwrap();
        std::fs::write(outside.path().join("escaped.py"), "").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("escaped.py"),
            project.path().join("escaped.py"),
        )
        .unwrap();
        let resolver = Resolver::new(
            project.path(),
            vec![dependencies.path().into()],
            Default::default(),
        )
        .unwrap();
        let allowed = resolver.lookup("allowed").unwrap().unwrap();
        assert_eq!(allowed[0].id, "python:allowed");
        assert!(allowed[0].external);
        assert!(resolver
            .lookup("escaped")
            .unwrap_err()
            .message
            .contains("escaped"));
    }
}
