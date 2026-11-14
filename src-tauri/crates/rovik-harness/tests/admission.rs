use rovik_harness::{
    ledger::Ledger,
    policy::EffectivePolicy,
    scope::Scope,
    snapshot::{self, AdmissionRules, FileEntry, Snapshot},
};

#[test]
fn complete_tree_admission_checks_deletions_renames_modes_and_build_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("state")).unwrap();
    let source = ledger
        .store("source", "application/octet-stream", b"original", 0)
        .unwrap();
    let base = Snapshot {
        schema_version: 1,
        base_revision: "base".into(),
        files: [
            (
                "src/a.rs".into(),
                FileEntry {
                    mode: 0o100644,
                    content: source.clone(),
                },
            ),
            (
                "acceptance/oracle.rs".into(),
                FileEntry {
                    mode: 0o100644,
                    content: source.clone(),
                },
            ),
        ]
        .into(),
        deletions: vec![],
        dependency_assets: vec![],
    };
    let policy = EffectivePolicy {
        commands: Default::default(),
        obligations: Default::default(),
        limits: Default::default(),
        facts: Default::default(),
        scopes: vec![Scope {
            allow_write: vec!["**".into()],
            protected: vec!["acceptance/**".into()],
        }],
        deny_read: vec![],
    };
    let rules = AdmissionRules::default();
    assert!(snapshot::admit(&ledger, &base, &base, &policy, &rules)
        .unwrap()
        .is_empty());
    for change in 0..6 {
        let mut candidate = base.clone();
        match change {
            0 => {
                let old = candidate.files.remove("acceptance/oracle.rs").unwrap();
                candidate.files.insert("src/moved.rs".into(), old);
                candidate.deletions.push("acceptance/oracle.rs".into());
            }
            1 => candidate.files.get_mut("src/a.rs").unwrap().mode = 0o100755,
            2 => {
                candidate.files.insert(
                    "Cargo.toml".into(),
                    FileEntry {
                        mode: 0o100644,
                        content: source.clone(),
                    },
                );
            }
            3 => {
                candidate.files.remove("src/a.rs");
            }
            4 => {
                candidate.files.insert(
                    "src/A.rs".into(),
                    FileEntry {
                        mode: 0o100644,
                        content: source.clone(),
                    },
                );
            }
            5 => candidate.files.get_mut("src/a.rs").unwrap().mode = 0o120000,
            _ => unreachable!(),
        }
        assert!(
            snapshot::admit(&ledger, &base, &candidate, &policy, &rules).is_err(),
            "change {change}"
        );
    }
    let mut candidate = base.clone();
    candidate.files.remove("src/a.rs");
    candidate.deletions.push("src/a.rs".into());
    assert_eq!(
        snapshot::admit(&ledger, &base, &candidate, &policy, &rules).unwrap(),
        vec!["src/a.rs"]
    );
    let materialized = dir.path().join("snapshot");
    base.materialize(&ledger, &materialized).unwrap();
    assert_eq!(
        std::fs::read(materialized.join("src/a.rs")).unwrap(),
        b"original"
    );
    assert!(base.materialize(&ledger, &materialized).is_err());
}
