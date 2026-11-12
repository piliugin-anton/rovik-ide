//! Registered argv commands and typed parameters. No model-generated shell strings.

use crate::{id, identity::Digest, plan::CheckKind, require, scope::validate_path, Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Trusted parser implementation selected at registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Parser {
    /// Static checks whose exit status is their entire result contract.
    ExitCode,
    /// Stable Rust test binary output with names and a complete summary.
    RustTests,
    /// CTest's structured JUnit report with exact test inventory.
    CtestJunit,
    /// Protected driver emitting the versioned inventory protocol.
    InventoryJson,
}

/// Parameter types validate entire argv elements before process launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Parameter {
    /// A bounded nonnegative integer.
    Integer {
        /// Inclusive lower bound.
        minimum: u64,
        /// Inclusive upper bound.
        maximum: u64,
    },
    /// An exact member of a trusted allowlist.
    Choice {
        /// Registered values.
        values: BTreeSet<String>,
    },
    /// A path under a fixed sandbox mount, never a host path.
    Path {
        /// One of /snapshot, /oracle, /build, /outputs or /dependencies.
        root: String,
    },
}
impl Parameter {
    fn validate(&self, value: &str) -> Result<()> {
        require(
            !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control),
            "invalid command parameter",
        )?;
        match self {
            Self::Integer { minimum, maximum } => {
                require(
                    value.bytes().all(|b| b.is_ascii_digit()),
                    "parameter must be an unsigned integer",
                )?;
                let n = value
                    .parse::<u64>()
                    .map_err(|_| Error("integer parameter overflow".into()))?;
                require(
                    minimum <= &n && &n <= maximum,
                    "integer parameter outside approved bounds",
                )
            }
            Self::Choice { values } => require(
                values.contains(value),
                "parameter is not an approved choice",
            ),
            Self::Path { root } => {
                require(
                    [
                        "/snapshot",
                        "/oracle",
                        "/build",
                        "/outputs",
                        "/dependencies",
                    ]
                    .contains(&root.as_str()),
                    "unregistered sandbox path root",
                )?;
                let relative = value
                    .strip_prefix(&format!("{root}/"))
                    .ok_or_else(|| Error("parameter escapes sandbox mount".into()))?;
                validate_path(relative)
            }
        }
    }
}

/// Upper execution bounds owned by the trusted command definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Wall-clock limit including compiler children.
    pub timeout_ms: u64,
    /// Combined stdout/stderr and each structured report bound.
    pub output_bytes: u64,
    /// Container memory, with swap disabled.
    pub memory_bytes: u64,
    /// Whole-container process limit.
    pub processes: u32,
    /// CPU quota in thousandths of one core.
    pub cpu_millis: u32,
    /// Maximum writable build space.
    pub build_bytes: u64,
}
impl Limits {
    /// No unlimited resources may authorize final execution.
    pub fn validate(&self) -> Result<()> {
        require(
            self.timeout_ms > 0
                && self.timeout_ms <= 86_400_000
                && self.output_bytes > 0
                && self.output_bytes <= 64 * 1024 * 1024
                && self.memory_bytes >= 16 * 1024 * 1024
                && self.processes > 0
                && self.cpu_millis > 0
                && self.build_bytes > 0,
            "invalid or unbounded sandbox limits",
        )
    }
}

/// A trusted command definition, registered outside the candidate repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandDefinition {
    /// Exactly 1.
    pub schema_version: u32,
    /// Stable command name.
    pub id: String,
    /// Absolute executable inside the pinned image or protected oracle mount.
    pub executable: String,
    /// Literal arguments or whole-element placeholders such as `{jobs}`.
    pub argv_template: Vec<String>,
    /// Exact parameter schema.
    pub parameters: BTreeMap<String, Parameter>,
    /// Fixed sandbox working directory.
    pub cwd: String,
    /// Explicit environment; everything else is cleared before repository code runs.
    pub environment: BTreeMap<String, String>,
    /// Trusted result semantics.
    pub kind: CheckKind,
    /// Deterministic native normalizer.
    pub parser: Parser,
    /// Exact checker implementation identity, including protected driver assets.
    pub checker_digest: Digest,
    /// Optional report under /outputs. Behavioral JSON/XML parsers require one.
    pub report_file: Option<String>,
    /// Bounded execution profile.
    pub limits: Limits,
}
impl CommandDefinition {
    /// Validate trusted registration, including placeholders and parser semantics.
    pub fn validate(&self) -> Result<()> {
        require(
            self.schema_version == 1,
            "unsupported command-registry version",
        )?;
        id(&self.id)?;
        require(
            self.executable.starts_with('/')
                && !self.executable.starts_with("/snapshot/")
                && !self.executable.starts_with("/build/"),
            "command executable must belong to pinned tools or protected assets",
        )?;
        validate_path(self.executable.trim_start_matches('/'))?;
        require(
            ["/snapshot", "/oracle", "/build", "/outputs"].contains(&self.cwd.as_str()),
            "invalid sandbox working directory",
        )?;
        self.limits.validate()?;
        require(
            self.checker_digest == crate::normalizer::checker_digest(self.parser),
            "registered checker does not match this controller's normalizer",
        )?;
        let mut used = BTreeSet::new();
        for arg in &self.argv_template {
            require(!arg.contains('\0'), "NUL in registered argument")?;
            if let Some(key) = arg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                require(
                    self.parameters.contains_key(key),
                    "unknown argv placeholder",
                )?;
                used.insert(key.to_string());
            }
        }
        require(
            used == self.parameters.keys().cloned().collect(),
            "unused registered parameters",
        )?;
        for (name, value) in &self.environment {
            require(
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|c| c.is_ascii_uppercase() || c == b'_' || c.is_ascii_digit())
                    && !value.contains('\0'),
                "invalid registered environment",
            )?;
            require(
                ![
                    "LD_PRELOAD",
                    "LD_LIBRARY_PATH",
                    "RUSTC_WRAPPER",
                    "RUSTC_WORKSPACE_WRAPPER",
                    "BASH_ENV",
                    "ENV",
                    "DOCKER_HOST",
                    "HTTP_PROXY",
                    "HTTPS_PROXY",
                    "ALL_PROXY",
                ]
                .contains(&name.as_str()),
                "forbidden execution environment variable",
            )?;
        }
        require(
            (self.kind == CheckKind::Static) == (self.parser == Parser::ExitCode),
            "behavioral command cannot use exit-code-only normalization",
        )?;
        if matches!(self.parser, Parser::InventoryJson | Parser::CtestJunit) {
            require(
                self.report_file.is_some(),
                "structured parser requires a report",
            )?;
        }
        if let Some(report) = &self.report_file {
            Parameter::Path {
                root: "/outputs".into(),
            }
            .validate(report)?;
        }
        Ok(())
    }
    /// Render parameters into separate argv elements; no shell interpolation occurs.
    pub fn render(&self, parameters: &BTreeMap<String, String>) -> Result<Vec<String>> {
        self.validate()?;
        require(
            parameters.keys().eq(self.parameters.keys()),
            "command parameters do not match the registry",
        )?;
        for (name, schema) in &self.parameters {
            schema.validate(&parameters[name])?;
        }
        let mut argv = vec![self.executable.clone()];
        for arg in &self.argv_template {
            argv.push(
                if let Some(key) = arg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                    parameters[key].clone()
                } else {
                    arg.clone()
                },
            );
        }
        Ok(argv)
    }
}

/// Immutable allowlist constructed by trusted onboarding.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRegistry {
    /// Versioned command definitions; duplicate IDs are rejected.
    pub commands: Vec<CommandDefinition>,
}
impl CommandRegistry {
    /// Validate all definitions before policy resolution or execution.
    pub fn validate(&self) -> Result<()> {
        crate::unique(self.commands.iter().map(|c| c.id.as_str()))?;
        for c in &self.commands {
            c.validate()?;
        }
        Ok(())
    }
    /// Resolve a known command without falling back to arbitrary execution.
    pub fn get(&self, id: &str) -> Result<&CommandDefinition> {
        self.validate()?;
        self.commands
            .iter()
            .find(|c| c.id == id)
            .ok_or_else(|| Error(format!("unregistered command: {id}")))
    }
}
