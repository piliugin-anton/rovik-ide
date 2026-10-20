//! The failures the daemon's handlers answer with (`Fail`), made in one
//! place.

use rovik_protocol::{Fail, FailCode};

/// A request the daemon can't take as it is.
pub fn bad_request(message: impl Into<String>) -> Fail {
    Fail::new(FailCode::BadRequest, message)
}

/// Something went wrong on the daemon's side.
pub fn internal(message: impl Into<String>) -> Fail {
    Fail::new(FailCode::Internal, message)
}

/// A git operation failed.
pub fn git(e: git2::Error) -> Fail {
    internal(e.message())
}

/// A blocking task's thread panicked, or was cancelled.
pub fn joined(e: tokio::task::JoinError) -> Fail {
    internal(e.to_string())
}
