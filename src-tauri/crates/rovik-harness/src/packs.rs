//! Native language policy adapters, sharing registry and receipt semantics.

use crate::{
    normalizer,
    plan::CheckKind,
    profile::{Language, Profile},
    registry::{CommandDefinition, Limits, Parser},
    require, Result,
};

/// Construct the dependency-free Rust reference consumer command.
/// Production profiles register their own locked workspace commands.
pub fn rust_reference_command(profile: &Profile) -> Result<CommandDefinition> {
    profile.require_resolved()?;
    require(
        profile.language == Language::Rust
            && profile.facts.get("edition").is_some_and(|s| s == "2021")
            && profile.configurations.len() == 1
            && profile.configurations[0].target == "x86_64-unknown-linux-gnu",
        "reference Rust consumer only supports its declared edition and target",
    )?;
    let config = &profile.configurations[0];
    require(
        config.runtime == "linux-native"
            && config.features.is_empty()
            && !config.default_features
            && config.build_targets == ["protected-consumer"]
            && config.flags
                == [
                    "--edition=2021",
                    "-Cpanic=unwind",
                    "-Coverflow-checks=yes",
                    "-Dwarnings",
                    "-Funsafe-code",
                ]
            && profile
                .facts
                .get("minimum_supported_rust_version")
                .map(String::as_str)
                == Some("1.85.1")
            && profile.facts.get("execution_target").map(String::as_str)
                == Some("x86_64-unknown-linux-gnu")
            && profile.facts.get("std_or_no_std").map(String::as_str) == Some("std")
            && profile.facts.get("unsafe_scope").map(String::as_str) == Some("forbidden")
            && profile.facts.get("panic_policy").map(String::as_str)
                == Some("unwind in the protected test consumer")
            && profile.facts.get("allocation_policy").map(String::as_str)
                == Some("no allocation in quota"),
        "reference Rust consumer cannot establish the selected flags or facts",
    )?;
    Ok(CommandDefinition {
        schema_version: 1,
        id: "quota-rust-consumer".into(),
        executable: "/bin/sh".into(),
        argv_template: vec!["/oracle/run.sh".into()],
        parameters: Default::default(),
        cwd: "/build".into(),
        environment: [
            ("RUSTUP_HOME".into(), "/usr/local/rustup".into()),
            ("CARGO_HOME".into(), "/usr/local/cargo".into()),
        ]
        .into(),
        kind: CheckKind::Behavioral,
        parser: Parser::InventoryJson,
        checker_digest: normalizer::checker_digest(Parser::InventoryJson),
        report_file: Some("/outputs/cases.json".into()),
        limits: Limits {
            timeout_ms: 60_000,
            output_bytes: 1_048_576,
            memory_bytes: 512 * 1024 * 1024,
            processes: 64,
            cpu_millis: 2000,
            build_bytes: 128 * 1024 * 1024,
        },
    })
}

/// Register configured graph checks for an explicit set of protected request IDs.
pub fn graph_command(requests: std::collections::BTreeSet<String>) -> Result<CommandDefinition> {
    require(
        !requests.is_empty(),
        "graph command needs protected requests",
    )?;
    for request in &requests {
        crate::id(request)?;
    }
    let mut command = native_command("native-configured-graph", Parser::PackProtocol);
    command.executable = crate::tools::EXECUTABLE.into();
    command.argv_template = vec!["pack".into(), "{request_id}".into()];
    command.parameters.insert(
        "request_id".into(),
        crate::registry::Parameter::Choice { values: requests },
    );
    command.kind = CheckKind::Static;
    command.report_file = None;
    Ok(command)
}

/// Register protected Node.js, TypeScript or Python tests with complete native reports.
pub fn runtime_test_command(
    requests: std::collections::BTreeSet<String>,
) -> Result<CommandDefinition> {
    let mut command = graph_command(requests)?;
    command.id = "runtime-protected-tests".into();
    command.parser = Parser::PackTests;
    command.checker_digest = normalizer::checker_digest(command.parser);
    command.kind = CheckKind::Behavioral;
    command.environment.extend([
        ("PYTEST_DISABLE_PLUGIN_AUTOLOAD".into(), "1".into()),
        ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
        ("PIP_CONFIG_FILE".into(), "/dev/null".into()),
    ]);
    Ok(command)
}
fn native_command(id: &str, parser: Parser) -> CommandDefinition {
    CommandDefinition {
        schema_version: 1,
        id: id.into(),
        executable: crate::tools::EXECUTABLE.into(),
        argv_template: vec!["quota".into()],
        parameters: Default::default(),
        cwd: "/build".into(),
        environment: [
            ("RUSTUP_HOME".into(), "/usr/local/rustup".into()),
            ("CARGO_HOME".into(), "/build/cargo".into()),
            ("CARGO_TARGET_DIR".into(), "/build/target".into()),
        ]
        .into(),
        kind: CheckKind::Behavioral,
        parser,
        checker_digest: normalizer::checker_digest(parser),
        report_file: Some("/outputs/cases.json".into()),
        limits: Limits {
            timeout_ms: 180_000,
            output_bytes: 4 * 1024 * 1024,
            memory_bytes: 1024 * 1024 * 1024,
            processes: 64,
            cpu_millis: 2000,
            build_bytes: 512 * 1024 * 1024,
        },
    }
}
/// GCC 12 hosted C17 / C++23 reference API consumer; no implicit target or feature defaults.
pub fn c_family_reference_command(profile: &Profile) -> Result<CommandDefinition> {
    profile.require_resolved()?;
    require(
        profile.configurations.len() == 1,
        "reference consumer needs one exact configuration",
    )?;
    let config = &profile.configurations[0];
    require(
        config.target == "x86_64-linux-gnu"
            && config.runtime == "linux-native"
            && config.features.is_empty()
            && !config.default_features
            && config.build_targets == ["protected-consumer"],
        "unsupported reference consumer configuration",
    )?;
    let cpp = profile.language == Language::Cpp;
    require(
        cpp || profile.language == Language::C,
        "expected C or C++ profile",
    )?;
    let expected: std::collections::BTreeMap<String, String> = if cpp {
        [
            ("standard_library_lock", "gcc-12-libstdc++-image-lock"),
            ("language_standard", "23"),
            ("target_abi", "x86_64-linux-gnu"),
            ("exceptions_policy", "disabled"),
            ("rtti_policy", "disabled"),
            ("allocation_policy", "no allocation in quota"),
            (
                "abi_compatibility_policy",
                "source-compatible public header",
            ),
            ("threading_model", "caller serialized"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
    } else {
        [
            ("language_standard", "17"),
            ("target_abi", "x86_64-linux-gnu"),
            ("hosted_or_freestanding", "hosted"),
            ("allocation_policy", "no allocation in quota"),
            (
                "threading_and_interrupt_policy",
                "caller serialized; no interrupts",
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
    };
    require(
        profile.facts == expected,
        "reference consumer does not establish these language facts",
    )?;
    let mut flags = vec!["-Wall".to_string(), "-Wextra".into(), "-Werror".into()];
    if cpp {
        flags.extend(["-fno-exceptions".into(), "-fno-rtti".into()]);
    }
    require(
        config.flags == flags,
        "reference consumer flags differ from its supported matrix",
    )?;
    Ok(native_command(
        if cpp {
            "quota-cpp-consumer"
        } else {
            "quota-c-consumer"
        },
        Parser::InventoryJson,
    ))
}
