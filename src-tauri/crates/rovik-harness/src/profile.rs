//! Explicit target profiles. File extensions never establish tool support.

use crate::{id, identity::Digest, require, unique, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// First-release native language families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    /// Cargo and rustc.
    Rust,
    /// CMake and a registered C++ compiler.
    Cpp,
    /// CMake and a registered C compiler.
    C,
}
impl Language {
    /// Facts that must be explicit before this profile can authorize execution.
    pub fn required_fields(self) -> &'static [&'static str] {
        match self {
            Self::Rust => &[
                "edition",
                "minimum_supported_rust_version",
                "execution_target",
                "std_or_no_std",
                "allocation_policy",
                "panic_policy",
                "unsafe_scope",
            ],
            Self::Cpp => &[
                "standard_library_lock",
                "language_standard",
                "target_abi",
                "exceptions_policy",
                "rtti_policy",
                "allocation_policy",
                "abi_compatibility_policy",
                "threading_model",
            ],
            Self::C => &[
                "language_standard",
                "target_abi",
                "hosted_or_freestanding",
                "allocation_policy",
                "threading_and_interrupt_policy",
            ],
        }
    }
}

/// Templates are draft guidance only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    /// Cannot authorize a run.
    Template,
    /// All required facts and immutable assets are present.
    Resolved,
}

/// A concrete member of the supported target/feature matrix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    /// Human-readable configuration ID.
    pub id: String,
    /// Native target identity, never an implicit host default.
    pub target: String,
    /// Explicit features or preprocessor definitions.
    pub features: Vec<String>,
    /// Whether default features are enabled.
    pub default_features: bool,
    /// Registered build target IDs.
    pub build_targets: Vec<String>,
    /// Approved compiler flags, including instrumentation where required.
    pub flags: Vec<String>,
    /// Registered native/device/emulator runtime.
    pub runtime: String,
}
impl Configuration {
    /// Exact identity of flags, features, runtime and target.
    pub fn digest(&self) -> Result<Digest> {
        Digest::of_json(self)
    }
}

/// Resolved native policy and complete supported configuration matrix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Exactly 1.
    pub schema_version: u32,
    /// Registered profile identifier.
    pub id: String,
    /// Native pack family.
    pub language: Language,
    /// Draft versus executable status.
    pub status: Resolution,
    /// Exact pinned toolchain manifest.
    pub toolchain_lock: Option<Digest>,
    /// Immutable, prepared dependency inputs.
    pub dependency_bundle: Option<Digest>,
    /// All language-specific facts required by the selected pack.
    pub facts: BTreeMap<String, String>,
    /// Complete required target/feature/build matrix.
    pub configurations: Vec<Configuration>,
    /// Protected consumers to compile and execute.
    pub public_consumer_commands: Vec<String>,
}
impl Profile {
    /// Templates, floating facts, incomplete matrices and unknown keys block execution.
    pub fn require_resolved(&self) -> Result<()> {
        require(self.schema_version == 1, "unsupported profile version")?;
        id(&self.id)?;
        require(
            self.status == Resolution::Resolved,
            "template profile cannot authorize execution",
        )?;
        require(
            self.toolchain_lock.is_some() && self.dependency_bundle.is_some(),
            "profile lacks pinned toolchain or dependency bundle",
        )?;
        for key in self.language.required_fields() {
            require(
                self.facts.get(*key).is_some_and(|v| {
                    !v.trim().is_empty()
                        && !["latest", "stable", "requires_resolution", "template"]
                            .contains(&v.as_str())
                }),
                format!("unresolved profile fact: {key}"),
            )?;
        }
        require(
            self.facts
                .keys()
                .all(|k| self.language.required_fields().contains(&k.as_str())),
            "unknown profile fact",
        )?;
        require(
            !self.configurations.is_empty(),
            "profile has no configurations",
        )?;
        unique(self.configurations.iter().map(|c| c.id.as_str()))?;
        for c in &self.configurations {
            id(&c.target)?;
            id(&c.runtime)?;
            require(
                !c.build_targets.is_empty(),
                "configuration has no build targets",
            )?;
            unique(c.features.iter().map(String::as_str))?;
            unique(c.build_targets.iter().map(String::as_str))?;
        }
        unique(self.public_consumer_commands.iter().map(String::as_str))
    }
}
