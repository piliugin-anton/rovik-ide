#![cfg(target_os = "linux")]
mod support;
use rovik_harness::{
    approvals::Principal,
    conformance::{ExpectedCheck, Fixture, Suite},
    controller::Controller,
    evidence::Status,
    fixtures,
    identity::Digest,
    ledger::Ledger,
    profile::Language,
    runner::{self, Cancellation, ContainerBackend},
};
fn suite(c: &Controller, image: Digest) -> Suite {
    let config = fixtures::quota_runtime(
        &c.ledger,
        "conformance-fixture",
        image,
        "base",
        Language::Python,
    )
    .unwrap();
    let bad = config.base.clone();
    let mut good = bad.clone();
    let (path, source) = fixtures::runtime_quota_source(Language::Python).unwrap();
    good.files.get_mut(path).unwrap().content = c
        .ledger
        .store(
            "source",
            "text/plain",
            source.as_bytes(),
            runner::now_ms().unwrap(),
        )
        .unwrap();
    let setup = c.ledger.store_json("operation", &config, 0).unwrap().digest;
    let expected = |status| {
        config
            .plan
            .required_final()
            .map(|check| ExpectedCheck {
                check_id: check.check_id.clone(),
                configuration_digest: check.configuration_digest.clone(),
                status,
                failed_case_ids: if status == Status::Fail {
                    config.baseline.expected_failures[0].case_ids.clone()
                } else {
                    Default::default()
                },
            })
            .collect()
    };
    Suite {
        schema_version: 1,
        suite_id: "python-pack-v1".into(),
        fixtures: vec![
            Fixture {
                fixture_id: "correct".into(),
                setup: setup.clone(),
                candidate: c.ledger.store_json("snapshot", &good, 0).unwrap().digest,
                expected: expected(Status::Pass),
            },
            Fixture {
                fixture_id: "wrong-boundary".into(),
                setup,
                candidate: c.ledger.store_json("snapshot", &bad, 0).unwrap().digest,
                expected: expected(Status::Fail),
            },
        ],
    }
}
#[test]
fn conformance_requires_positive_and_negative_evidence_for_the_complete_matrix() {
    let dir = tempfile::tempdir().unwrap();
    let c = Controller {
        ledger: Ledger::open(&dir.path().join("state")).unwrap(),
        backend: ContainerBackend::local(std::path::Path::new("/bin/true")).unwrap(),
    };
    let mut suite = suite(&c, Digest::of(b"image"));
    let principal = Principal::current().unwrap();
    suite.validate(&c, &principal).unwrap();
    let removed = suite.fixtures.pop().unwrap();
    assert!(suite.validate(&c, &principal).is_err());
    suite.fixtures.push(removed);
    suite.fixtures[1].expected[0].status = Status::Error;
    assert!(suite.validate(&c, &principal).is_err());
    suite.fixtures[1].expected[0].status = Status::Fail;
    suite.fixtures[0].expected[0].configuration_digest = Digest::of(b"undeclared-configuration");
    assert!(suite.validate(&c, &principal).is_err());
}
#[test]
#[ignore = "requires ROVIK_HARNESS_DOCKER=1 and the pinned runtime image"]
fn native_conformance_reports_the_real_passing_consumer_and_rejected_boundary_mutant() {
    assert_eq!(std::env::var("ROVIK_HARNESS_DOCKER").unwrap(), "1");
    let dir = tempfile::tempdir().unwrap();
    let mut c = Controller {
        ledger: Ledger::open(&dir.path().join("state")).unwrap(),
        backend: ContainerBackend::local(&support::docker_path()).unwrap(),
    };
    let suite = suite(
        &c,
        Digest::parse(&std::env::var("ROVIK_HARNESS_IMAGE").unwrap()).unwrap(),
    );
    let report = c
        .pack_conformance(
            &Principal::current().unwrap(),
            &suite,
            &Cancellation::default(),
        )
        .unwrap();
    assert!(report.conforms, "{report:?}");
    assert_eq!(report.fixtures.len(), 2);
    for fixture in report.fixtures {
        assert!(fixture.matches);
        assert_eq!(fixture.results[0].executed_test_count, 7);
        assert_eq!(
            c.ledger.budgets(&fixture.run_id).unwrap()["model_calls"].spent,
            0
        );
    }
}

#[test]
fn malformed_or_misidentified_protected_requests_fail_before_native_execution() {
    use rovik_harness::{pack_protocol::Request, wire};
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open(&dir.path().join("state")).unwrap();
    let config = fixtures::quota_runtime(
        &ledger,
        "request-preflight",
        Digest::of(b"image"),
        "base",
        Language::Python,
    )
    .unwrap();
    let bytes = ledger
        .read(&config.oracle.files["requests/quota.json"].content.digest)
        .unwrap();
    let mut wrong: Request = wire::decode(&bytes).unwrap();
    wrong.request_id = "another-request".into();
    let mut misidentified = wire::canonical(&wrong).unwrap();
    misidentified.push(b'\n');
    for (bytes, message) in [
        (bytes[..bytes.len() - 1].to_vec(), "must end with newline"),
        (misidentified, "identity mismatch"),
    ] {
        let mut invalid = config.clone();
        invalid
            .oracle
            .files
            .get_mut("requests/quota.json")
            .unwrap()
            .content = ledger
            .store("source", "application/json", &bytes, 0)
            .unwrap();
        invalid.plan.oracle_digest = invalid.oracle.digest().unwrap();
        let error = invalid.validate(&ledger).unwrap_err();
        assert!(error.message.contains(message), "{error}");
    }
}
