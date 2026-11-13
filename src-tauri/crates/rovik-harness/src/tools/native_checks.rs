//! Native workspace drivers execute only registered command families inside the sandbox.
use super::{args, checked, checked_source, copy_tree, run};
use crate::{
    native_checks::{NativeCheck, NativeCheckInput, NativeCheckOutput},
    normalizer::{self, Inventory},
    profile::{Configuration, Language},
    require, wire, Error, Result,
};
use serde_json::Value;
use std::{collections::BTreeSet, path::Path};

fn append(destination: &mut Inventory, source: Inventory, prefix: &str) {
    for (to, from) in [
        (&mut destination.discovered, source.discovered),
        (&mut destination.selected, source.selected),
        (&mut destination.passed, source.passed),
        (&mut destination.failed, source.failed),
        (&mut destination.skipped, source.skipped),
    ] {
        to.extend(from.into_iter().map(|id| format!("{prefix}::{id}")));
    }
}
fn discovered(output: &str) -> Result<BTreeSet<String>> {
    let names: BTreeSet<_> = output
        .lines()
        .filter_map(|line| line.strip_suffix(": test"))
        .map(str::to_owned)
        .collect();
    for name in &names {
        crate::id(name)?;
    }
    Ok(names)
}
fn config_args(config: &Configuration) -> Vec<String> {
    let mut flags = args(&["--locked", "--offline", "--target", &config.target]);
    if !config.default_features {
        flags.push("--no-default-features".into());
    }
    if !config.features.is_empty() {
        flags.extend(["--features".into(), config.features.join(",")]);
    }
    flags
}
fn cargo(input: &NativeCheckInput, config: &Configuration) -> Result<(Inventory, i32)> {
    let root = Path::new("/build/workspace");
    copy_tree(Path::new("/snapshot"), root)?;
    let cwd = root.join(&input.native.profile.build_root);
    if Path::new("/dependencies/cargo").is_dir() {
        copy_tree(Path::new("/dependencies/cargo"), Path::new("/build/cargo"))?;
    }
    // Encoded flags preserve argument boundaries without a shell or whitespace splitting.
    let flags = config.flags.join("\u{1f}");
    // Rustdoc executes temporary test binaries; the runner permits execution only in /build.
    std::fs::create_dir_all("/build/tmp")?;
    let environment = [
        ("TMPDIR", "/build/tmp"),
        ("CARGO_ENCODED_RUSTFLAGS", flags.as_str()),
        ("CARGO_ENCODED_RUSTDOCFLAGS", flags.as_str()),
    ];
    let metadata: Value = wire::decode(
        checked(
            &args(&[
                "/usr/local/cargo/bin/cargo",
                "metadata",
                "--locked",
                "--offline",
                "--format-version=1",
                "--no-deps",
            ]),
            &cwd,
        )?
        .as_bytes(),
    )?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| Error::new("Cargo packages missing".into()))?;
    for package in packages {
        require(
            package["edition"].as_str()
                == input
                    .native
                    .profile
                    .facts
                    .get("edition")
                    .map(String::as_str),
            "workspace edition differs from registered profile",
        )?;
    }
    if matches!(
        input.check,
        NativeCheck::CargoFmt | NativeCheck::CargoClippy
    ) {
        let installed = checked(
            &args(&[
                "/usr/local/cargo/bin/rustup",
                "component",
                "list",
                "--installed",
            ]),
            &cwd,
        )?;
        let component = if input.check == NativeCheck::CargoFmt {
            "rustfmt-"
        } else {
            "clippy-"
        };
        require(
            installed.lines().any(|line| line.starts_with(component)),
            "required Rust component is missing",
        )
        .map_err(|e| e.with_category(crate::FailureCategory::Capability))?;
    }
    let mut inventory = Inventory {
        schema_version: 1,
        ..Default::default()
    };
    let compiler = "/usr/local/cargo/bin/cargo";
    let invoke = |command: &[String]| run(command, &cwd, &environment);
    if matches!(
        input.check,
        NativeCheck::CargoFmt | NativeCheck::CargoCheck | NativeCheck::CargoClippy
    ) {
        let mut command = args(&[
            compiler,
            match input.check {
                NativeCheck::CargoFmt => "fmt",
                NativeCheck::CargoClippy => "clippy",
                _ => "check",
            },
        ]);
        if input.check == NativeCheck::CargoFmt {
            command.extend(args(&["--all", "--", "--check"]));
        } else {
            command.extend(args(&["--workspace", "--all-targets"]));
            command.extend(config_args(config));
            if input.check == NativeCheck::CargoClippy {
                command.extend(args(&["--", "-D", "warnings"]));
            }
        }
        let output = invoke(&command)?;
        std::io::Write::write_all(&mut std::io::stderr(), &output.stdout)?;
        return Ok((
            inventory,
            output.code.ok_or_else(|| {
                Error::new("native compiler terminated without exit status".into())
            })?,
        ));
    }
    if input.check == NativeCheck::CargoTests {
        let mut command = args(&[
            compiler,
            "test",
            "--workspace",
            "--all-targets",
            "--no-run",
            "--message-format=json",
        ]);
        command.extend(config_args(config));
        let output = invoke(&command)?;
        require(output.code == Some(0), "workspace test compilation failed")
            .map_err(|e| e.with_category(crate::FailureCategory::Implementation))?;
        let mut binaries = BTreeSet::new();
        for line in output
            .stdout
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let artifact: Value = wire::decode(line)?;
            if artifact["reason"] != "compiler-artifact" || artifact["profile"]["test"] != true {
                continue;
            }
            if let Some(executable) = artifact["executable"].as_str() {
                require(
                    Path::new(executable).starts_with("/build/target"),
                    "test binary escaped build output",
                )?;
                let package = artifact["package_id"]
                    .as_str()
                    .ok_or_else(|| Error::new("test artifact has no package".into()))?;
                let target = artifact["target"]["name"]
                    .as_str()
                    .ok_or_else(|| Error::new("test artifact has no target".into()))?;
                let package = packages
                    .iter()
                    .find(|item| item["id"] == package)
                    .ok_or_else(|| {
                        Error::new("test artifact references an unknown package".into())
                    })?;
                let name = package["name"]
                    .as_str()
                    .ok_or_else(|| Error::new("package name missing".into()))?;
                let version = package["version"]
                    .as_str()
                    .ok_or_else(|| Error::new("package version missing".into()))?;
                let kinds = artifact["target"]["kind"]
                    .as_array()
                    .ok_or_else(|| Error::new("target kind missing".into()))?
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("+");
                binaries.insert((
                    format!("{name}@{version}/{kinds}/{target}"),
                    executable.to_string(),
                ));
            }
        }
        require(!binaries.is_empty(), "workspace compiled no test binaries")?;
        for (identity, executable) in binaries {
            let listing = checked(&args(&[&executable, "--list", "--format=terse"]), &cwd)?;
            let expected = discovered(&listing)?;
            let output = invoke(&args(&[&executable, "--format=pretty", "--test-threads=1"]))?;
            if output.code != Some(0) {
                std::io::Write::write_all(&mut std::io::stderr(), &output.stdout)?;
            }
            let mut cases = normalizer::parse_rust(&output.stdout)?;
            require(
                cases.selected.iter().cloned().collect::<BTreeSet<_>>() == expected,
                "workspace test inventory changed between discovery and execution",
            )?;
            require(
                output.code == Some(0) || !cases.failed.is_empty(),
                "test process failed without complete failing cases",
            )?;
            cases.discovered = expected.into_iter().collect();
            append(&mut inventory, cases, &identity);
        }
    } else {
        let metadata: Value = wire::decode(
            checked(
                &args(&[
                    compiler,
                    "metadata",
                    "--locked",
                    "--offline",
                    "--format-version=1",
                    "--no-deps",
                ]),
                &cwd,
            )?
            .as_bytes(),
        )?;
        let members: BTreeSet<_> = metadata["workspace_members"]
            .as_array()
            .ok_or_else(|| Error::new("Cargo workspace members missing".into()))?
            .iter()
            .filter_map(Value::as_str)
            .collect();
        for package in metadata["packages"]
            .as_array()
            .ok_or_else(|| Error::new("Cargo packages missing".into()))?
        {
            if !package["id"]
                .as_str()
                .is_some_and(|id| members.contains(id))
            {
                continue;
            }
            if !package["targets"]
                .as_array()
                .is_some_and(|targets| targets.iter().any(|t| t["doctest"] == true))
            {
                continue;
            }
            let name = package["name"]
                .as_str()
                .ok_or_else(|| Error::new("Cargo package name missing".into()))?;
            let mut command = args(&[compiler, "test", "--doc", "--package", name]);
            command.extend(config_args(config));
            let mut listing = command.clone();
            listing.extend(args(&["--", "--list", "--format=terse"]));
            let output = invoke(&listing)?;
            require(output.code == Some(0), "doctest discovery failed")
                .map_err(|e| e.with_category(crate::FailureCategory::Implementation))?;
            let expected = discovered(
                std::str::from_utf8(&output.stdout).map_err(|e| Error::new(e.to_string()))?,
            )?;
            command.extend(args(&["--", "--format=pretty", "--test-threads=1"]));
            let output = invoke(&command)?;
            if output.code != Some(0) {
                std::io::Write::write_all(&mut std::io::stderr(), &output.stdout)?;
            }
            let mut cases = normalizer::parse_rust(&output.stdout)?;
            require(
                cases.selected.iter().cloned().collect::<BTreeSet<_>>() == expected,
                "doctest discovery does not match execution",
            )?;
            require(
                output.code == Some(0) || !cases.failed.is_empty(),
                "doctest process failed without complete failing cases",
            )?;
            cases.discovered = expected.into_iter().collect();
            append(&mut inventory, cases, &format!("{name}/doc"));
        }
    }
    inventory.validate()?;
    let code = i32::from(!inventory.failed.is_empty());
    Ok((inventory, code))
}
/// Execute a closed workspace operation after validating its exact native profile.
pub(super) fn check(input: &NativeCheckInput) -> Result<NativeCheckOutput> {
    input.validate()?;
    let (config, compiler_version) = super::native::configuration(&input.native)?;
    let (inventory, exit_code) = if input.native.profile.language == Language::Rust {
        cargo(input, config)?
    } else {
        let graph = super::native::cmake_graph(
            input.native.profile.language,
            config,
            &input.native.profile.build_root,
        )?;
        let mut build = args(&["/usr/bin/cmake", "--build", "/build/native", "--target"]);
        build.extend(config.build_targets.clone());
        checked_source(&build, Path::new("/build"))?;
        if input.check == NativeCheck::ClangTidy {
            for unit in &graph.compiler_commands {
                let mut args = args(&[
                    "/usr/bin/clang-tidy",
                    &format!("/snapshot/{}", unit.file),
                    "--config={Checks: '-*,clang-analyzer-*,bugprone-*', WarningsAsErrors: '*'}",
                    "--",
                ]);
                let mut skip = false;
                for argument in unit.argv.iter().skip(1) {
                    if skip {
                        skip = false;
                        continue;
                    }
                    if argument == "-o" {
                        skip = true;
                    } else if argument != "-c" && !argument.ends_with(&unit.file) {
                        args.push(if argument == "-std=c++23" {
                            "-std=c++2b".into()
                        } else {
                            argument.clone()
                        });
                    }
                }
                checked_source(&args, Path::new("/build/native"))?;
            }
            (
                Inventory {
                    schema_version: 1,
                    ..Default::default()
                },
                0,
            )
        } else {
            let output = run(
                &args(&[
                    "/usr/bin/ctest",
                    "--test-dir",
                    "/build/native",
                    "--output-junit",
                    "/outputs/workspace.xml",
                    "--output-on-failure",
                ]),
                Path::new("/build"),
                &[],
            )?;
            let mut inventory = normalizer::parse_junit(&std::fs::read("/outputs/workspace.xml")?)?;
            require(
                inventory.selected.iter().collect::<BTreeSet<_>>()
                    == graph.test_inventory.iter().collect::<BTreeSet<_>>(),
                "CTest omitted configured cases",
            )?;
            inventory.discovered = graph.test_inventory;
            (
                inventory,
                output
                    .code
                    .ok_or_else(|| Error::new("CTest terminated without status".into()))?,
            )
        }
    };
    Ok(NativeCheckOutput {
        check: input.check,
        configuration_digest: config.digest()?,
        compiler_version,
        inventory,
        exit_code,
    })
}
