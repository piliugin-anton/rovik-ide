//! The Rust wire contract is the source for frontend types and agent type guidance.

use super::{StagedFile, Submission};
use crate::harness::ide_prepare::*;
use rovik_harness::{
    controller::WorkerLimits,
    profile::{Configuration, Language},
    Error, Result,
};
use ts_rs::TS;

/// Generate the preparation input types and required language facts without writing files.
/// `scripts/generate-preparation-types.mjs` explicitly updates the checked-in frontend artifact.
pub fn typescript_preparation_contract() -> Result<String> {
    let mut output = String::from("// Generated from Rust preparation contracts. Run npm run generate:preparation-types to update.\n\n");
    for declaration in [
        Language::decl(),
        Configuration::decl(),
        WorkerLimits::decl(),
        WorkspaceBuildExclusion::decl(),
        WorkspaceProfileInput::decl(),
        WorkspaceRequirement::decl(),
        WorkspaceCitation::decl(),
        WorkspaceDesignInput::decl(),
        WorkspaceEnvironmentInput::decl(),
        WorkspaceBaselineFailure::decl(),
        WorkspaceSetupInput::decl(),
        StagedFile::decl(),
        Submission::decl(),
    ] {
        output.push_str("// prettier-ignore\nexport ");
        output.push_str(&declaration);
        output.push_str("\n\n");
    }
    let facts = [
        Language::Rust,
        Language::C,
        Language::Cpp,
        Language::JavaScript,
        Language::TypeScript,
        Language::Python,
    ]
    .into_iter()
    .map(|language| (language, language.required_fields()))
    .collect::<std::collections::BTreeMap<_, _>>();
    output.push_str("// prettier-ignore\nexport const PROFILE_FACTS = ");
    output.push_str(
        &serde_json::to_string_pretty(&facts).map_err(|error| Error::new(error.to_string()))?,
    );
    output.push_str(" as const satisfies Record<Language, readonly string[]>;\n");
    Ok(format!(
        "{}\n",
        output
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontend_preparation_contract_matches_the_rust_input_types() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../src/lib/generated/preparation.ts");
        let saved = std::fs::read_to_string(path).unwrap();
        assert_eq!(
            // Git may convert the checked-in artifact to CRLF on Windows.
            // Normalize only line endings so all contract changes still fail.
            saved.replace("\r\n", "\n"),
            typescript_preparation_contract().unwrap(),
            "Run npm run generate:preparation-types and commit the result."
        );
    }
}
