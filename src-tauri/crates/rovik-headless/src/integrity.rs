//! P02 (docs/design/evolve-plan.md): the grader's integrity flags for a
//! run's change, from the files it changed (what each was at the run's
//! start, and is now), by `repair::grading_flags`. Run as its own binary
//! (`rovik-integrity`), built from the campaign's commit, so the build
//! being evaluated can't weaken what grades it.
//!
//! Input (stdin): `{ "files": [ { "path": "a.py", "before": "…" | null,
//! "after": "…" | null } ] }`; output (stdout): `{ "flags": [ { "path":
//! "a.py", "flags": ["…"] } ] }`, the flagged files only, in input order.

use rovik_core::agent_run::repair::grading_flags;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
struct Input {
    files: Vec<FileChange>,
}

/// One file the run changed: None before, it's new; None after, deleted.
#[derive(Deserialize)]
struct FileChange {
    path: String,
    before: Option<String>,
    after: Option<String>,
}

/// The flags of the changed files in `input` (the JSON above).
pub fn flags(input: &str) -> Result<Value, String> {
    let input: Input = serde_json::from_str(input).map_err(|e| format!("bad input: {e}"))?;
    let flagged: Vec<Value> = input
        .files
        .iter()
        .map(|f| {
            let flags = grading_flags(&f.path, f.before.as_deref(), f.after.as_deref());
            (f, flags)
        })
        .filter(|(_, flags)| !flags.is_empty())
        .map(|(f, flags)| json!({ "path": f.path, "flags": flags }))
        .collect();
    Ok(json!({ "flags": flagged }))
}

/// The binary's entry: stdin to stdout; 1 for input it can't read.
pub fn main_cli() -> i32 {
    let mut input = String::new();
    if let Err(e) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input) {
        eprintln!("rovik-integrity: {e}");
        return 1;
    }
    match flags(&input) {
        Ok(out) => {
            println!("{out}");
            0
        }
        Err(e) => {
            eprintln!("rovik-integrity: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flagged_files_come_back_and_the_rest_dont() {
        let input = json!({ "files": [
            { "path": "src/more.py", "before": "def f():\n    return 0\n", "after": "def f():\n    return 1\n" },
            { "path": "tests/conftest.py", "before": null,
              "after": "def pytest_collection_modifyitems(items):\n    items[:] = []\n" },
            { "path": "tests/test_old.py", "before": "def test_a():\n    assert 1\n", "after": null },
        ]});
        let out = flags(&input.to_string()).unwrap();
        let paths: Vec<&str> = out["flags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["path"].as_str().unwrap())
            .collect();
        assert_eq!(paths, ["tests/conftest.py", "tests/test_old.py"]);
        assert!(out["flags"][1]["flags"][0]
            .as_str()
            .unwrap()
            .contains("deleted, with 1 test"));
        assert!(flags("{}").unwrap_err().starts_with("bad input"));
    }
}
