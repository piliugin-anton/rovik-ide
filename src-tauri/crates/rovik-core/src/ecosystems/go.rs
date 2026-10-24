//! Go: modules with a `go.mod`, and the download of what its `go.sum`
//! pins where nothing is vendored. A cloud host doesn't install Go: its
//! setup script does.

use super::{Ecosystem, Install, Project};
use crate::toolchains::Want;

/// Go's modules ([`Ecosystem`]).
pub struct Go;

impl Ecosystem for Go {
    fn name(&self) -> &'static str {
        "Go"
    }

    fn programs(&self) -> &'static [&'static str] {
        &["go", "gofmt"]
    }

    fn present(&self, project: &Project) -> bool {
        project.with("go.mod").next().is_some()
    }

    fn toolchain(&self, _project: &Project) -> Option<Want> {
        None
    }

    fn installs(&self, project: &Project) -> Vec<Install> {
        project
            .with("go.mod")
            .filter(|folder| project.has(folder, "go.sum") && !project.has(folder, "vendor"))
            .map(|folder| Install {
                folder: folder.to_string(),
                command: "go mod download".into(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{at, commands, project};

    #[test]
    fn go_downloads_its_modules_where_nothing_is_vendored() {
        assert_eq!(
            commands(&project(&["go.mod", "go.sum"]).0),
            [at("", "go mod download")]
        );
        for files in [&["go.mod", "go.sum", "vendor/"][..], &["go.mod"]] {
            assert_eq!(commands(&project(files).0), [], "{files:?}");
        }
    }
}
