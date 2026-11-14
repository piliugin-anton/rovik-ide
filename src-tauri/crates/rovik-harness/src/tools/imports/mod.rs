//! Static project-module boundaries, with native resolution and opaque external leaves.
mod python;
mod scan;

use super::{args, checked_source, runtime, write_json};
use crate::{
    pack_protocol::{Edge, Graph, NativeInput, Node, Operation, Precision},
    profile::Language,
    require, Error, Result,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    id: String,
    external: bool,
}

pub(super) fn sources(
    root: &Path,
    directory: &Path,
    python: bool,
    files: &mut BTreeMap<String, String>,
    bytes: &mut usize,
) -> Result<()> {
    require(
        directory
            .strip_prefix(root)
            .map_err(|e| Error::new(e.to_string()))?
            .components()
            .count()
            <= 64,
        "import directory nesting exceeds limit",
    )?;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        require(
            kind.is_dir() || kind.is_file(),
            "import inventory cannot contain symlinks or special files",
        )?;
        if kind.is_dir() {
            require(
                entry.file_name() != "node_modules",
                "candidate node_modules is unsupported; use prepared dependencies",
            )?;
            sources(root, &entry.path(), python, files, bytes)?;
            continue;
        }
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|e| Error::new(e.to_string()))?
            .to_str()
            .ok_or_else(|| Error::new("non-UTF-8 import path".into()))?
            .to_string();
        crate::scope::validate_path(&relative)?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let code = if python {
            ext == "py"
        } else {
            matches!(
                ext,
                "js" | "mjs" | "cjs" | "jsx" | "ts" | "tsx" | "mts" | "cts"
            )
        };
        if code {
            require(
                entry.metadata()?.len() <= 2 * 1024 * 1024,
                "source exceeds import analysis limit",
            )?;
            let content = std::fs::read_to_string(&path)?;
            *bytes += content.len();
            require(
                files.len() < 4096 && *bytes <= 32 * 1024 * 1024,
                "project exceeds import analysis limits",
            )?;
            files.insert(relative, content);
        }
    }
    Ok(())
}

fn emitted_modules(files: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>> {
    let directory = Path::new(runtime::PROJECT).join("dist");
    if !directory.exists() {
        return Ok(BTreeMap::new());
    }
    let mut emitted = BTreeMap::new();
    sources(&directory, &directory, false, &mut emitted, &mut 0)?;
    let mut mapping = BTreeMap::new();
    for path in emitted.keys() {
        let path = Path::new(path);
        let extensions: &[&str] = match path.extension().and_then(|ext| ext.to_str()) {
            Some("mjs") => &["mts", "mjs"],
            Some("cjs") => &["cts", "cjs"],
            Some("js" | "jsx") => &["ts", "tsx", "js", "jsx"],
            _ => continue, // Declaration output is not executable JavaScript.
        };
        let candidates: Vec<_> = extensions
            .iter()
            .map(|ext| path.with_extension(ext).to_string_lossy().into_owned())
            .filter(|path| files.contains_key(path))
            .collect();
        require(
            candidates.len() == 1,
            "emitted module needs one parsed source",
        )?;
        mapping.insert(format!("dist/{}", path.display()), candidates[0].clone());
    }
    Ok(mapping)
}

pub(super) fn index(input: &NativeInput, operation: Operation) -> Result<Value> {
    let config = crate::runtime::configuration(
        &input.profile,
        &input.configuration_id,
        input.typescript_project.as_deref(),
    )?;
    if operation == Operation::Probe {
        let versions = runtime::versions(input.profile.language)?;
        require(
            versions
                .iter()
                .all(|(key, value)| input.profile.facts.get(key) == Some(value)),
            "import graph tool versions mismatch",
        )?;
        return Ok(
            json!({"language":input.profile.language,"versions":versions,"capabilities":["project-imports"],"configuration_digest":config.digest()?}),
        );
    }
    let python = input.profile.language == Language::Python;
    let mut files = BTreeMap::new();
    let source_root = Path::new("/snapshot").join(&input.profile.build_root);
    sources(&source_root, &source_root, python, &mut files, &mut 0)?;
    require(!files.is_empty(), "no project modules found")?;
    let mut imports = Vec::new();
    for (path, source) in &files {
        imports.extend(scan::parse(path, source, python).map_err(|e| {
            Error::classified(
                crate::FailureCategory::Implementation,
                format!("{path}: {e}"),
            )
        })?);
        require(imports.len() <= 65536, "too many project imports")?;
    }
    let versions = runtime::prepare(&input.profile, input.typescript_project.as_deref())?;
    let mut emitted = BTreeMap::new();
    if input.profile.language == Language::TypeScript {
        emitted = emitted_modules(&files)?;
        for (runtime_path, source_path) in &emitted {
            let source = std::fs::read_to_string(Path::new(runtime::PROJECT).join(runtime_path))?;
            for mut import in scan::parse(runtime_path, &source, false)? {
                import.source.clone_from(source_path);
                import.runtime_source = Some(runtime_path.clone());
                imports.push(import);
            }
        }
        require(imports.len() <= 65536, "too many project imports")?;
    }
    let targets = if python {
        python::resolve(&files, &imports)?
    } else {
        std::fs::write("/build/resolve-imports.mjs", include_str!("resolve.mjs"))?;
        write_json(
            "/build/imports.json",
            &json!({"imports":imports,"emitted":emitted,"typescript_project":input.typescript_project}),
        )?;
        let bytes = checked_source(
            &args(&[
                runtime::NODE,
                "--experimental-import-meta-resolve",
                "/build/resolve-imports.mjs",
                "/build/imports.json",
            ]),
            Path::new(runtime::PROJECT),
        )?;
        crate::wire::decode::<Vec<Vec<Target>>>(bytes.as_bytes())?
    };
    require(
        targets.len() == imports.len(),
        "incomplete import resolution",
    )?;
    let mut nodes: BTreeMap<_, _> = files
        .keys()
        .map(|path| {
            (
                path.clone(),
                Node {
                    id: path.clone(),
                    path: Some(path.clone()),
                    kind: "module".into(),
                },
            )
        })
        .collect();
    let mut edges = BTreeSet::new();
    for (import, targets) in imports.iter().zip(targets) {
        require(
            !targets.is_empty(),
            format!("unresolved import {} in {}", import.module, import.source),
        )?;
        for target in targets {
            if !target.external {
                crate::scope::validate_path(&target.id)?;
                let path = Path::new(runtime::PROJECT).join(&target.id);
                require(
                    files.contains_key(&target.id)
                        || (python && path.is_dir())
                        || (!python && path.extension().is_some_and(|ext| ext == "json")),
                    format!(
                        "imported code is outside the parsed inventory: {}",
                        target.id
                    ),
                )?;
            }
            nodes.entry(target.id.clone()).or_insert_with(|| Node {
                id: target.id.clone(),
                path: if target.external {
                    None
                } else {
                    Some(target.id.clone())
                },
                kind: if target.external { "external" } else { "asset" }.into(),
            });
            edges.insert((import.source.clone(), target.id));
        }
    }
    if python {
        for path in files.keys() {
            for parent in Path::new(path)
                .ancestors()
                .skip(1)
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                let init = parent.join("__init__.py").to_string_lossy().into_owned();
                if &init != path && files.contains_key(&init) {
                    edges.insert((path.clone(), init));
                }
            }
        }
    }
    let mut graph = Graph {
        precision: Precision::ProjectImports,
        nodes: nodes.into_values().collect(),
        edges: edges
            .into_iter()
            .map(|(source, target)| Edge {
                source,
                target,
                kind: "import".into(),
            })
            .collect(),
        unresolved: vec![],
        configuration_digest: config.digest()?,
        compiler_commands: vec![],
        test_inventory: vec![],
    };
    // The controller independently applies the approved boundaries to this observed graph.
    if !input.profile.build_root.is_empty() {
        let local: BTreeSet<_> = graph
            .nodes
            .iter()
            .filter(|node| node.path.is_some())
            .map(|node| node.id.clone())
            .collect();
        let prefix = |path: &str| format!("{}/{path}", input.profile.build_root);
        for node in &mut graph.nodes {
            if let Some(path) = &mut node.path {
                *path = prefix(path);
                node.id = prefix(&node.id);
            }
        }
        for edge in &mut graph.edges {
            if local.contains(&edge.source) {
                edge.source = prefix(&edge.source);
            }
            if local.contains(&edge.target) {
                edge.target = prefix(&edge.target);
            }
        }
    }
    Ok(json!({"language":input.profile.language,"graph":graph,"versions":versions}))
}
