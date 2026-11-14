//! Typed failure attribution. Only trusted adapters choose repairable outcomes.

use serde::{Deserialize, Serialize};

/// Why an operation failed, independently of its native exit status.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCategory {
    /// Invalid user input or a denied policy request.
    InvalidInput,
    /// Candidate compilation, behavior or architecture must be repaired.
    Implementation,
    /// Ownership or representation needs a reviewed design revision.
    Design,
    /// A business fact is missing or conflicts with the approved contract.
    Contract,
    /// A protected capability or policy boundary was violated.
    Policy,
    /// A supported operation failed before it produced trustworthy evidence.
    #[default]
    Infrastructure,
    /// The registered environment cannot perform the required check.
    Capability,
    /// An explicit execution or spending limit was reached.
    Budget,
    /// The operator cancelled the operation.
    Cancelled,
    /// An unexpected controller or durable-storage failure.
    Internal,
}

/// A validation or operational error with an explicit recovery category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// Human-readable reason, excluding credentials and unbounded native output.
    pub message: String,
    /// Trusted attribution used for CLI outcomes and bounded recovery.
    pub category: FailureCategory,
}

impl Error {
    /// Invalid input is the conservative default for domain validation errors.
    pub fn new(message: String) -> Self {
        Self {
            message,
            category: FailureCategory::InvalidInput,
        }
    }

    /// Attribute an error at the adapter which knows what actually failed.
    pub fn classified(category: FailureCategory, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            category,
        }
    }

    /// Preserve the explanation while assigning an adapter-established category.
    pub fn with_category(mut self, category: FailureCategory) -> Self {
        self.category = category;
        self
    }

    /// Management-command exit status; execution reports use their durable state.
    pub fn exit_code(&self) -> i32 {
        match self.category {
            FailureCategory::InvalidInput | FailureCategory::Policy => 4,
            FailureCategory::Implementation | FailureCategory::Budget => 3,
            FailureCategory::Infrastructure
            | FailureCategory::Capability
            | FailureCategory::Design
            | FailureCategory::Contract => 2,
            FailureCategory::Cancelled => 5,
            FailureCategory::Internal => 1,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_exit_codes_distinguish_invalid_input_from_controller_failure() {
        assert_eq!(Error::new("bad profile".into()).exit_code(), 4);
        assert_eq!(
            Error::classified(FailureCategory::Internal, "database failed").exit_code(),
            1
        );
        assert_eq!(
            Error::classified(FailureCategory::Infrastructure, "runner unavailable").exit_code(),
            2
        );
        assert_eq!(
            Error::classified(FailureCategory::Implementation, "compiler error").exit_code(),
            3
        );
        assert_eq!(
            Error::classified(FailureCategory::Cancelled, "cancelled").exit_code(),
            5
        );
    }
}
