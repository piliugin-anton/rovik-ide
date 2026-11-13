//! Closed native workspace operations, with independent discovery and outcome validation.
use crate::{
    evidence::Status,
    identity::Digest,
    normalizer::{Inventory, Normalized},
    pack_protocol::{self, NativeInput, Operation, Request},
    profile::Language,
    require, wire, Error, Result,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Registered build/test operations. No worker-supplied argv is executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeCheck {
    /// Check formatting across the selected Cargo workspace.
    CargoFmt,
    /// Compile every workspace target under the selected features and target.
    CargoCheck,
    /// Run Clippy with warnings denied across all workspace targets.
    CargoClippy,
    /// Discover and execute every compiled workspace test binary independently.
    CargoTests,
    /// Discover and execute doctests separately for every workspace package.
    CargoDocTests,
    /// Analyze every configured translation unit using pinned Clang-Tidy checks.
    ClangTidy,
    /// Build selected CMake targets and execute the complete CTest inventory.
    Ctest,
}
impl NativeCheck {
    /// Whether this operation must produce behavioral case observations.
    pub fn behavioral(self) -> bool {
        matches!(self, Self::CargoTests | Self::CargoDocTests | Self::Ctest)
    }
}
/// Protected workspace check and exact native configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCheckInput {
    /// Resolved build root, toolchain, features and discovery requirements.
    pub native: NativeInput,
    /// Closed operation selection.
    pub check: NativeCheck,
}
impl NativeCheckInput {
    /// Reject language/operation mismatches before executing candidate build code.
    pub fn validate(&self) -> Result<()> {
        Request {
            protocol_version: 1,
            request_id: "workspace-validation".into(),
            operation: Operation::Index,
            payload: serde_json::to_value(&self.native).map_err(|e| Error::new(e.to_string()))?,
        }
        .native_input()?;
        require(
            matches!(
                (self.native.profile.language, self.check),
                (
                    Language::Rust,
                    NativeCheck::CargoFmt
                        | NativeCheck::CargoCheck
                        | NativeCheck::CargoClippy
                        | NativeCheck::CargoTests
                        | NativeCheck::CargoDocTests
                ) | (
                    Language::C | Language::Cpp,
                    NativeCheck::ClangTidy | NativeCheck::Ctest
                )
            ),
            "native operation does not support this language",
        )?;
        if !self.check.behavioral() {
            require(
                self.native.required_test_ids.is_empty(),
                "static operation cannot claim behavioral cases",
            )?;
        }
        Ok(())
    }
}
/// Complete image-side observation, independently checked by the controller.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCheckOutput {
    /// Native check actually performed.
    pub check: NativeCheck,
    /// Exact selected configuration.
    pub configuration_digest: Digest,
    /// Observed compiler version, from the pinned execution image.
    pub compiler_version: String,
    /// Separately discovered and executed cases; static checks have none.
    pub inventory: Inventory,
    /// Native execution outcome, correlated with the adapter process exit.
    pub exit_code: i32,
}
/// Decode one protected native workspace request.
pub fn input(request: &Request) -> Result<NativeCheckInput> {
    require(
        request.protocol_version == 1 && request.operation == Operation::NativeCheck,
        "wrong native workspace operation",
    )?;
    let input: NativeCheckInput = wire::decode(&wire::canonical(&request.payload)?)?;
    input.validate()?;
    Ok(input)
}
/// Validate protocol correlation, matrix identity and complete discovery before normalization.
pub fn verify(
    request: &Request,
    bytes: &[u8],
    configuration: &Digest,
    exit: Option<i32>,
) -> Result<Normalized> {
    let input = input(request)?;
    let response = pack_protocol::response(request, bytes)?;
    response.require_success()?;
    let output: NativeCheckOutput = wire::decode(&wire::canonical(&response.result)?)?;
    let config = input
        .native
        .profile
        .configurations
        .iter()
        .find(|config| config.id == input.native.configuration_id)
        .ok_or_else(|| Error::new("native configuration disappeared".into()))?;
    require(
        &output.configuration_digest == configuration
            && &config.digest()? == configuration
            && output.check == input.check
            && !output.compiler_version.is_empty()
            && exit == Some(output.exit_code),
        "native observation identity mismatch",
    )?;
    output.inventory.validate()?;
    require(
        output.inventory.discovered.iter().collect::<BTreeSet<_>>()
            == output.inventory.selected.iter().collect::<BTreeSet<_>>(),
        "workspace verification omitted discovered cases",
    )?;
    require(
        input
            .native
            .required_test_ids
            .iter()
            .all(|id| output.inventory.discovered.contains(id)),
        "required workspace test disappeared",
    )?;
    require(
        input.check.behavioral() || output.inventory.discovered.is_empty(),
        "static check fabricated test cases",
    )?;
    let passed = output.exit_code == 0
        && output.inventory.failed.is_empty()
        && output.inventory.skipped.is_empty();
    require(
        output.exit_code != 0 || output.inventory.failed.is_empty(),
        "native success conflicts with failing cases",
    )?;
    Ok(Normalized {
        status: if passed { Status::Pass } else { Status::Fail },
        failure_category: (!passed).then_some(crate::FailureCategory::Implementation),
        inventory: output.inventory,
        diagnostic: (!passed)
            .then(|| "native workspace check failed or skipped required cases".into()),
    })
}
