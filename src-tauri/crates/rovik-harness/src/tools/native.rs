//! Configured Cargo/CMake adapter; protocol types are shared with the controller.
use super::{args, checked, checked_source, copy_tree, read, LIMIT};
use crate::{
    pack_protocol::{
        CompilerCommand, Edge, Graph, NativeInput, Node, Operation, PackError, Precision, Request,
        Response,
    },
    profile::{Configuration, Language},
    require, wire, Error, Result,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    path::{Path, PathBuf},
};

const ROOT: &str = "/snapshot";
const BUILD: &str = "/build/native";
fn text(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| Error::new(format!("native report lacks string {key}")))
}
fn list<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| Error::new(format!("native report lacks array {key}")))
}
fn execute(values: &[&str]) -> Result<String> {
    checked(&args(values), Path::new(ROOT))
}
fn relative(value: &str) -> Result<String> {
    let path = Path::new(value);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        Path::new(ROOT).join(path)
    }
    .canonicalize()?;
    for (root, prefix) in [
        (ROOT, ""),
        ("/oracle", "oracle:"),
        ("/build", "generated:"),
        ("/dependencies", "dependency:"),
    ] {
        if let Ok(relative) = path.strip_prefix(root) {
            return Ok(format!(
                "{prefix}{}",
                relative.to_string_lossy().replace('\\', "/")
            ));
        }
    }
    Err(Error::new(
        "graph references an undeclared source root".into(),
    ))
}
fn configuration(input: &NativeInput) -> Result<(&Configuration, String)> {
    let profile = &input.profile;
    profile.require_resolved()?;
    let config = profile
        .configurations
        .iter()
        .find(|c| c.id == input.configuration_id)
        .ok_or_else(|| Error::new("unknown configuration".into()))?;
    require(
        config.runtime == "linux-native",
        "unsupported native runtime",
    )?;
    let version = match profile.language {
        Language::Rust => {
            let version = execute(&["/usr/local/cargo/bin/rustc", "--version"])?;
            require(
                version.split_whitespace().nth(1)
                    == Some(profile.facts["minimum_supported_rust_version"].as_str()),
                "MSRV needs its own registered compiler",
            )?;
            require(
                execute(&[
                    "/usr/local/cargo/bin/rustup",
                    "target",
                    "list",
                    "--installed",
                ])?
                .lines()
                .any(|v| v == config.target),
                "target standard library missing",
            )?;
            require(
                profile.facts["std_or_no_std"] == "std",
                "no_std requires a target runner",
            )?;
            version.trim().into()
        }
        Language::C | Language::Cpp => {
            let compiler = if profile.language == Language::C {
                "/usr/bin/gcc"
            } else {
                "/usr/bin/g++"
            };
            require(
                execute(&[compiler, "-dumpversion"])?.trim() == "12",
                "unregistered GCC version",
            )?;
            require(
                execute(&[compiler, "-dumpmachine"])?.trim() == config.target,
                "unregistered compiler target",
            )?;
            require(
                profile.facts["language_standard"]
                    == if profile.language == Language::C {
                        "17"
                    } else {
                        "23"
                    },
                "unsupported language standard",
            )?;
            execute(&[compiler, "--version"])?
                .lines()
                .next()
                .unwrap_or_default()
                .into()
        }
        _ => {
            return Err(Error::new(
                "runtime packs support protected tests, not configured native graphs".into(),
            ))
        }
    };
    Ok((config, version))
}
fn rust_graph(config: &Configuration) -> Result<Graph> {
    if Path::new("/dependencies/cargo").is_dir() {
        copy_tree(Path::new("/dependencies/cargo"), Path::new("/build/cargo"))?;
    }
    let mut command = args(&[
        "/usr/local/cargo/bin/cargo",
        "metadata",
        "--format-version=1",
        "--locked",
        "--offline",
        "--filter-platform",
        &config.target,
    ]);
    if !config.default_features {
        command.push("--no-default-features".into());
    }
    if !config.features.is_empty() {
        command.extend(["--features".into(), config.features.join(",")]);
    }
    let metadata: Value = wire::decode(checked(&command, Path::new(ROOT))?.as_bytes())?;
    let mut labels = BTreeMap::new();
    let mut nodes = Vec::new();
    for package in list(&metadata, "packages")? {
        let identity = text(package, "id")?;
        let label = format!("{}@{}", text(package, "name")?, text(package, "version")?);
        labels.insert(identity, label.clone());
        nodes.push(Node {
            id: label,
            path: if package.get("source").is_none_or(Value::is_null) {
                Some(relative(&text(package, "manifest_path")?)?)
            } else {
                None
            },
            kind: "package".into(),
        });
    }
    require(
        labels.values().collect::<BTreeSet<_>>().len() == labels.len(),
        "ambiguous package versions require component mapping",
    )?;
    let resolve = metadata
        .get("resolve")
        .ok_or_else(|| Error::new("missing Cargo resolve graph".into()))?;
    let mut edges = Vec::new();
    for node in list(resolve, "nodes")? {
        let source = labels
            .get(&text(node, "id")?)
            .ok_or_else(|| Error::new("unresolved Cargo source".into()))?;
        for dep in list(node, "deps")? {
            let target = labels
                .get(&text(dep, "pkg")?)
                .ok_or_else(|| Error::new("unresolved Cargo dependency".into()))?;
            for kind in list(dep, "dep_kinds")? {
                edges.push(Edge {
                    source: source.clone(),
                    target: target.clone(),
                    kind: kind
                        .get("kind")
                        .and_then(Value::as_str)
                        .unwrap_or("normal")
                        .into(),
                });
            }
        }
    }
    Ok(Graph {
        precision: Precision::CargoResolved,
        nodes,
        edges,
        unresolved: vec![],
        configuration_digest: config.digest()?,
        compiler_commands: vec![],
        test_inventory: vec![],
    })
}
fn cmake_feature(value: &str) -> bool {
    let Some((name, value)) = value.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .bytes()
            .enumerate()
            .all(|(i, c)| c == b'_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))
        && (matches!(value, "ON" | "OFF")
            || (!value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())))
}
fn cmake_graph(language: Language, config: &Configuration) -> Result<Graph> {
    let build = Path::new(BUILD);
    require(
        !build.exists(),
        "build database must be collected in a fresh directory",
    )?;
    let query = build.join(".cmake/api/v1/query/codemodel-v2");
    std::fs::create_dir_all(
        query
            .parent()
            .ok_or_else(|| Error::new("invalid query path".into()))?,
    )?;
    std::fs::write(query, b"")?;
    let flags = shlex::try_join(config.flags.iter().map(String::as_str))
        .map_err(|e| Error::new(e.to_string()))?;
    let mut command = args(&[
        "/usr/bin/cmake",
        "-S",
        ROOT,
        "-B",
        BUILD,
        "-G",
        "Ninja",
        "-DCMAKE_BUILD_TYPE=Debug",
        "-DCMAKE_EXPORT_COMPILE_COMMANDS=ON",
        "-DCMAKE_C_COMPILER=/usr/bin/gcc",
        "-DCMAKE_CXX_COMPILER=/usr/bin/g++",
        "-DCMAKE_C_STANDARD=17",
        "-DCMAKE_CXX_STANDARD=23",
        "-DCMAKE_C_STANDARD_REQUIRED=ON",
        "-DCMAKE_CXX_STANDARD_REQUIRED=ON",
        "-DCMAKE_C_EXTENSIONS=OFF",
        "-DCMAKE_CXX_EXTENSIONS=OFF",
    ]);
    command.push(format!(
        "-DCMAKE_{}_FLAGS={flags}",
        if language == Language::C { "C" } else { "CXX" }
    ));
    for feature in &config.features {
        require(
            cmake_feature(feature),
            "unsupported CMake feature selection",
        )?;
        command.push(format!("-D{feature}"));
    }
    checked(&command, Path::new(ROOT))?;
    let mut command = args(&[
        "/usr/bin/cmake",
        "--build",
        BUILD,
        "--parallel",
        "2",
        "--target",
    ]);
    command.extend(config.build_targets.clone());
    checked_source(&command, Path::new(ROOT))?;
    let database: Vec<Value> = read(build.join("compile_commands.json"))?;
    require(!database.is_empty(), "empty compilation database")?;
    let mut nodes = BTreeMap::new();
    let mut edges = Vec::new();
    let mut commands = Vec::new();
    for unit in database {
        let argv = if let Some(arguments) = unit.get("arguments") {
            wire::decode::<Vec<String>>(&wire::canonical(arguments)?)?
        } else {
            shlex::split(&text(&unit, "command")?)
                .ok_or_else(|| Error::new("malformed compiler arguments".into()))?
        };
        require(
            argv.first().is_some_and(|v| {
                [
                    "/usr/bin/gcc",
                    "/usr/bin/g++",
                    "/usr/bin/cc",
                    "/usr/bin/c++",
                ]
                .contains(&v.as_str())
            }),
            "unregistered compiler",
        )?;
        let directory = PathBuf::from(text(&unit, "directory")?).canonicalize()?;
        require(directory.starts_with(build), "stale compilation directory")?;
        let source = relative(&text(&unit, "file")?)?;
        nodes.insert(
            source.clone(),
            Node {
                id: source.clone(),
                path: Some(source.clone()),
                kind: "translation_unit".into(),
            },
        );
        commands.push(CompilerCommand {
            file: source.clone(),
            argv: argv.clone(),
        });
        let mut preprocess = Vec::new();
        let mut skip = false;
        for arg in argv {
            if skip {
                skip = false;
                continue;
            }
            if ["-o", "-MF", "-MT", "-MQ"].contains(&arg.as_str()) {
                skip = true;
            } else if !["-c", "-MD", "-MMD", "-MP"].contains(&arg.as_str()) {
                preprocess.push(arg);
            }
        }
        preprocess.extend(args(&["-MM", "-MT", "__harness"]));
        let output = checked_source(&preprocess, &directory)?;
        let deps = output
            .strip_prefix("__harness:")
            .ok_or_else(|| Error::new("malformed compiler dependencies".into()))?;
        for included in shlex::split(&deps.replace("\\\n", " "))
            .ok_or_else(|| Error::new("malformed include paths".into()))?
        {
            let target = relative(&included)?;
            nodes.entry(target.clone()).or_insert(Node {
                id: target.clone(),
                path: Some(target.clone()),
                kind: "header".into(),
            });
            if source != target {
                edges.push(Edge {
                    source: source.clone(),
                    target,
                    kind: "include".into(),
                });
            }
        }
    }
    let replies = build.join(".cmake/api/v1/reply");
    let indices = std::fs::read_dir(&replies)?
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.starts_with("index-") && n.ends_with(".json")
        })
        .collect::<Vec<_>>();
    require(indices.len() == 1, "missing or ambiguous CMake reply")?;
    let index: Value = read(indices[0].path())?;
    let model_name = index
        .pointer("/reply/codemodel-v2/jsonFile")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::new("missing CMake codemodel".into()))?;
    crate::scope::validate_path(model_name)?;
    let model: Value = read(replies.join(model_name))?;
    let configs = list(&model, "configurations")?;
    require(configs.len() == 1, "ambiguous CMake configuration")?;
    let targets = list(&configs[0], "targets")?;
    let mut labels = BTreeMap::new();
    for target in targets {
        let name = format!("target:{}", text(target, "name")?);
        labels.insert(text(target, "id")?, name.clone());
        nodes.insert(
            name.clone(),
            Node {
                id: name,
                path: None,
                kind: "build_target".into(),
            },
        );
    }
    for target in targets {
        let name = labels
            .get(&text(target, "id")?)
            .ok_or_else(|| Error::new("unknown target".into()))?;
        let file = text(target, "jsonFile")?;
        crate::scope::validate_path(&file)?;
        let detail: Value = read(replies.join(file))?;
        for source in list(&detail, "sources")? {
            if source.get("compileGroupIndex").is_some() {
                let unit = relative(&text(source, "path")?)?;
                require(
                    nodes.contains_key(&unit),
                    "codemodel source absent from compiler database",
                )?;
                edges.push(Edge {
                    source: name.clone(),
                    target: unit,
                    kind: "compiles".into(),
                });
            }
        }
        if let Some(dependencies) = detail.get("dependencies") {
            for dependency in dependencies
                .as_array()
                .ok_or_else(|| Error::new("malformed CMake dependencies".into()))?
            {
                let to = labels
                    .get(&text(dependency, "id")?)
                    .ok_or_else(|| Error::new("unresolved CMake target".into()))?;
                edges.push(Edge {
                    source: name.clone(),
                    target: to.clone(),
                    kind: "link_or_build".into(),
                });
            }
        }
    }
    let tests: Value = wire::decode(
        execute(&["/usr/bin/ctest", "--test-dir", BUILD, "--show-only=json-v1"])?.as_bytes(),
    )?;
    Ok(Graph {
        precision: Precision::ConfiguredBuild,
        nodes: nodes.into_values().collect(),
        edges,
        unresolved: vec![],
        configuration_digest: config.digest()?,
        compiler_commands: commands,
        test_inventory: list(&tests, "tests")?
            .iter()
            .map(|t| text(t, "name"))
            .collect::<Result<_>>()?,
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Explanation {
    parser_version: u32,
    diagnostic: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposedPlan {
    plan: crate::plan::CheckPlan,
    registered_commands: BTreeSet<String>,
}
fn dispatch(request: &Request) -> Result<Value> {
    require(
        request.protocol_version == 1,
        "unsupported native pack protocol",
    )?;
    crate::id(&request.request_id)?;
    match request.operation {
        Operation::Test => {
            let input = crate::runtime::input(request)?;
            serde_json::to_value(super::runtime::check(&input)?)
                .map_err(|e| Error::new(e.to_string()))
        }
        Operation::Explain => {
            let input: Explanation = wire::decode(&wire::canonical(&request.payload)?)?;
            require(
                input.parser_version == 1
                    && !input.diagnostic.is_empty()
                    && input.diagnostic.len() <= 65536,
                "invalid diagnostic",
            )?;
            Ok(json!({"diagnostics":[{"rule_id":"native-log", "message": input.diagnostic}]}))
        }
        Operation::PlanChecks => {
            let input: ProposedPlan = wire::decode(&wire::canonical(&request.payload)?)?;
            require(
                input
                    .plan
                    .checks
                    .iter()
                    .all(|c| input.registered_commands.contains(&c.command_id)),
                "unregistered plan command",
            )?;
            Ok(json!({"plan":input.plan}))
        }
        Operation::Probe | Operation::Index => {
            let input = request.native_input()?;
            if !crate::runtime::version_keys(input.profile.language).is_empty() {
                return super::imports::index(&input, request.operation);
            }
            let (config, compiler) = configuration(&input)?;
            if request.operation == Operation::Probe {
                return Ok(
                    json!({"language":input.profile.language,"profile_resolution":"resolved","compiler":compiler,"configuration_digest":config.digest()?,"observed":{"compiler":compiler},"configured":input.profile.facts,"uncertain":[],"capabilities":["compile","protected-consumer",if input.profile.language==Language::Rust {"cargo-resolved-graph"}else{"configured-build-graph"}],"unsupported_obligations":[]}),
                );
            }
            let graph = if input.profile.language == Language::Rust {
                rust_graph(config)?
            } else {
                cmake_graph(input.profile.language, config)?
            };
            Ok(json!({"language":input.profile.language,"graph":graph}))
        }
    }
}
pub(super) fn protocol(request_id: &str) -> Result<i32> {
    let mut identity = "invalid-request".to_string();
    let mut test_exit = 0;
    let result = (|| -> Result<Value> {
        let bytes = if request_id == "-" {
            let mut data = Vec::new();
            std::io::stdin()
                .take((LIMIT + 1) as u64)
                .read_to_end(&mut data)?;
            data
        } else {
            crate::scope::validate_path(request_id)?;
            require(
                !request_id.contains('/'),
                "request ID must be a single path segment",
            )?;
            let mut data = Vec::new();
            std::fs::File::open(format!("/oracle/requests/{request_id}.json"))?
                .take((LIMIT + 1) as u64)
                .read_to_end(&mut data)?;
            data
        };
        require(bytes.ends_with(b"\n"), "request must end with newline")?;
        let request: Request = wire::decode(&bytes)?;
        identity = request.request_id.clone();
        if request_id != "-" {
            require(identity == request_id, "request ID mismatch")?;
        }
        let result = dispatch(&request)?;
        if request.operation == Operation::Test {
            let output: crate::runtime::TestOutput = wire::decode(&wire::canonical(&result)?)?;
            test_exit = output.exit_code;
        }
        Ok(result)
    })();
    let response = Response {
        protocol_version: 1,
        request_id: identity,
        ok: result.is_ok(),
        result: result.as_ref().ok().cloned(),
        error: result.as_ref().err().map(|e| PackError {
            code: if e.category == crate::FailureCategory::Implementation {
                "CANDIDATE_FAILURE"
            } else {
                "NATIVE_OPERATION_FAILED"
            }
            .into(),
            message: e.to_string(),
            retryable: e.category == crate::FailureCategory::Infrastructure,
            category: e.category,
        }),
    };
    let mut output = wire::canonical(&response)?;
    output.push(b'\n');
    require(output.len() <= LIMIT, "native response exceeds limit")?;
    std::io::stdout().write_all(&output)?;
    Ok(if result.is_ok() { test_exit } else { 2 })
}
