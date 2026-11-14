//! Package the verifier's own sources, never files from the user's project.
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    env, fs,
    path::{Path, PathBuf},
};
use toml::Value;

fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_PREPARATION");
    if env::var_os("CARGO_FEATURE_PREPARATION").is_none() {
        return;
    }
    if let Err(error) = bundle() {
        panic!("cannot bundle the trusted verification helper: {error}");
    }
}

fn read(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed={}", path.display());
    Ok(fs::read_to_string(path)?)
}

fn bundle() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let workspace = root
        .parent()
        .and_then(Path::parent)
        .ok_or("missing workspace")?;
    let out = PathBuf::from(env::var("OUT_DIR")?);
    let mut manifest: Value = toml::from_str(&read(&root.join("Cargo.toml"))?)?;
    let workspace_manifest: Value = toml::from_str(&read(&workspace.join("Cargo.toml"))?)?;
    let lock: Value = toml::from_str(&read(&workspace.join("Cargo.lock"))?)?;
    let package = manifest["package"]
        .as_table_mut()
        .ok_or("invalid package manifest")?;
    for (key, value) in package.iter_mut() {
        if value.get("workspace").and_then(Value::as_bool) == Some(true) {
            *value = workspace_manifest["workspace"]["package"][key].clone();
        }
    }
    resolve_dependencies(&mut manifest, &workspace_manifest)?;
    manifest
        .as_table_mut()
        .ok_or("invalid manifest")?
        .insert("workspace".into(), Value::Table(Default::default()));
    let standalone = out.join("preparation-Cargo.toml");
    fs::write(&standalone, toml::to_string(&manifest)?)?;
    let standalone_lock = out.join("preparation-Cargo.lock");
    let locked = prune_lock(&lock, &manifest)?;
    fs::write(&standalone_lock, toml::to_string(&locked)?)?;
    let index = locked["package"]
        .as_array()
        .ok_or("missing standalone packages")?
        .iter()
        .map(|package| {
            Ok((
                package["name"]
                    .as_str()
                    .ok_or("missing locked name")?
                    .to_string(),
                package["version"]
                    .as_str()
                    .ok_or("missing locked version")?
                    .to_string(),
                package
                    .get("source")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                package
                    .get("checksum")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            ))
        })
        .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
    let lock_index = out.join("preparation-lock-index.json");
    fs::write(&lock_index, serde_json::to_vec(&index)?)?;

    let mut files = BTreeMap::new();
    for directory in [
        "src",
        "migrations",
        "examples",
        "fixtures/reference",
        "fixtures/runtime",
        "fixtures/evaluation",
    ] {
        collect(&root, &root.join(directory), &mut files)?;
    }
    for name in [
        "build.rs",
        "fixtures/native/Dockerfile",
        "fixtures/native/clang-graph.cpp",
    ] {
        println!("cargo:rerun-if-changed={}", root.join(name).display());
        files.insert(name.into(), root.join(name));
    }
    files.insert("Cargo.toml".into(), standalone);
    files.insert("Cargo.lock".into(), standalone_lock);
    files.insert("trusted-lock.json".into(), lock_index);
    let mut generated = String::from("const SOURCE_FILES: &[(&str, &[u8])] = &[\n");
    for (name, path) in files {
        generated.push_str(&format!(
            "({name:?}, include_bytes!({:?})),\n",
            path.to_str().ok_or("non-UTF-8 verifier source path")?
        ));
    }
    generated.push_str("];\n");
    fs::write(out.join("preparation-sources.rs"), generated)?;
    Ok(())
}

fn collect(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<String, PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed={}", directory.display());
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        if entry.file_name() == "__pycache__"
            || path.extension().is_some_and(|extension| extension == "pyc")
        {
            continue;
        }
        if kind.is_dir() {
            collect(root, &path, files)?;
        } else if kind.is_file() {
            let relative = path
                .strip_prefix(root)?
                .to_str()
                .ok_or("non-UTF-8 verifier source")?
                .replace('\\', "/");
            files.insert(relative, path);
        } else {
            return Err(
                format!("verifier source is not a regular file: {}", path.display()).into(),
            );
        }
    }
    Ok(())
}

fn resolve_dependencies(
    manifest: &mut Value,
    workspace: &Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(table) = manifest.as_table_mut() else {
        return Ok(());
    };
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(dependencies) = table.get_mut(section).and_then(Value::as_table_mut) {
            for (name, value) in dependencies {
                if value.get("workspace").and_then(Value::as_bool) == Some(true) {
                    let mut base = workspace["workspace"]["dependencies"]
                        .get(name)
                        .ok_or("missing workspace dependency")?
                        .clone();
                    if let Value::String(version) = base {
                        base = Value::Table(
                            [(String::from("version"), Value::String(version))]
                                .into_iter()
                                .collect(),
                        );
                    }
                    for (key, option) in value.as_table().ok_or("invalid inherited dependency")? {
                        if key != "workspace" {
                            base.as_table_mut()
                                .ok_or("invalid workspace dependency")?
                                .insert(key.clone(), option.clone());
                        }
                    }
                    *value = base;
                }
                if value.get("path").is_some() || value.get("git").is_some() {
                    return Err(format!(
                        "the standalone verifier dependency {name} needs a registry lock"
                    )
                    .into());
                }
            }
        }
    }
    if let Some(targets) = table.get_mut("target").and_then(Value::as_table_mut) {
        for (_, target) in targets.iter_mut() {
            resolve_dependencies(target, workspace)?;
        }
    }
    Ok(())
}

fn prune_lock(lock: &Value, manifest: &Value) -> Result<Value, Box<dyn std::error::Error>> {
    let packages = lock["package"]
        .as_array()
        .ok_or("missing locked packages")?;
    let root = packages
        .iter()
        .position(|package| {
            package["name"] == manifest["package"]["name"]
                && package["version"] == manifest["package"]["version"]
        })
        .ok_or("verifier is missing from Cargo.lock")?;
    let mut wanted = BTreeSet::new();
    let mut queue = VecDeque::from([root]);
    while let Some(index) = queue.pop_front() {
        if !wanted.insert(index) {
            continue;
        }
        for dependency in packages[index]
            .get("dependencies")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let parts = dependency
                .as_str()
                .ok_or("invalid locked dependency")?
                .split_whitespace()
                .collect::<Vec<_>>();
            let choices = packages
                .iter()
                .enumerate()
                .filter(|(_, package)| {
                    package["name"].as_str() == parts.first().copied()
                        && parts
                            .get(1)
                            .is_none_or(|version| package["version"].as_str() == Some(*version))
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            if choices.len() != 1 {
                return Err(format!("ambiguous locked verifier dependency: {dependency}").into());
            }
            queue.push_back(choices[0]);
        }
    }
    Ok(Value::Table(
        [
            ("version".into(), lock["version"].clone()),
            (
                "package".into(),
                Value::Array(
                    wanted
                        .into_iter()
                        .map(|index| packages[index].clone())
                        .collect(),
                ),
            ),
        ]
        .into_iter()
        .collect(),
    ))
}
