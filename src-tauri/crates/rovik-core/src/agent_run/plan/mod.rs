//! Plan mode: it investigates without changing anything and ends with a
//! plan, saved with `submit_plan` as Markdown in `.rovik/plans/`, where it
//! can be read, edited and implemented. A revised plan in the same chat
//! updates the same file. The agent's questions are questions.rs's, and
//! its checklist is checklist.rs's.

mod check;
mod criteria;
mod doc;
mod file;
mod format;
mod prompt;
mod steps;
#[cfg(test)]
mod tests;
mod validation;

pub use check::*;
pub use criteria::*;
pub use doc::PlanDoc;
pub use file::*;
pub use format::*;
pub use prompt::*;
pub use steps::*;
pub use validation::*;
