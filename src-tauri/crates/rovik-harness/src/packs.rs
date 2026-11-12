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
