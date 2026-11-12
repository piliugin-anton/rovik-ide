PRAGMA foreign_keys = ON;
CREATE TABLE schema_versions(version INTEGER PRIMARY KEY, applied_ms INTEGER NOT NULL);
CREATE TABLE artifacts(
 digest TEXT PRIMARY KEY, kind TEXT NOT NULL, media_type TEXT NOT NULL,
 byte_count INTEGER NOT NULL CHECK(byte_count >= 0), created_ms INTEGER NOT NULL
);
CREATE TABLE runs(
 run_id TEXT PRIMARY KEY, state TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision >= 0),
 input_digest TEXT NOT NULL REFERENCES artifacts(digest),
 binding_digest TEXT REFERENCES artifacts(digest), restart_state TEXT,
 created_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL
);
CREATE TABLE events(
 run_id TEXT NOT NULL REFERENCES runs(run_id), sequence INTEGER NOT NULL,
 from_state TEXT NOT NULL, to_state TEXT NOT NULL, event_digest TEXT NOT NULL REFERENCES artifacts(digest),
 created_ms INTEGER NOT NULL, PRIMARY KEY(run_id, sequence)
);
CREATE TABLE operations(
 operation_id TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES runs(run_id), operation_key TEXT NOT NULL,
 kind TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('running','completed','failed','cancelled','abandoned')),
 input_digest TEXT NOT NULL REFERENCES artifacts(digest), output_digest TEXT REFERENCES artifacts(digest),
 generation INTEGER NOT NULL CHECK(generation >= 0), lease_owner TEXT NOT NULL,
 lease_expires_ms INTEGER NOT NULL, attempts INTEGER NOT NULL CHECK(attempts > 0),
 reservation_json TEXT NOT NULL, UNIQUE(run_id, operation_key)
);
CREATE TABLE receipts(
 receipt_id TEXT PRIMARY KEY, operation_id TEXT NOT NULL REFERENCES operations(operation_id),
 generation INTEGER NOT NULL, receipt_digest TEXT NOT NULL REFERENCES artifacts(digest),
 result_digest TEXT NOT NULL REFERENCES artifacts(digest), created_ms INTEGER NOT NULL,
 UNIQUE(operation_id, generation)
);
CREATE TABLE check_results(
 result_id TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES runs(run_id),
 operation_id TEXT NOT NULL UNIQUE REFERENCES operations(operation_id),
 check_id TEXT NOT NULL, configuration_digest TEXT NOT NULL,
 binding_digest TEXT NOT NULL REFERENCES artifacts(digest),
 result_digest TEXT NOT NULL REFERENCES artifacts(digest), receipt_id TEXT NOT NULL REFERENCES receipts(receipt_id),
 status TEXT NOT NULL CHECK(status IN ('pass','fail','error','timeout','unsupported','not_applicable')),
 created_ms INTEGER NOT NULL
);
CREATE TABLE approvals(
 approval_id TEXT PRIMARY KEY, run_id TEXT REFERENCES runs(run_id),
 subject_digest TEXT NOT NULL REFERENCES artifacts(digest), approval_type TEXT NOT NULL,
 actor_id TEXT NOT NULL, policy_digest TEXT NOT NULL REFERENCES artifacts(digest),
 scope_digest TEXT NOT NULL REFERENCES artifacts(digest), approval_digest TEXT NOT NULL REFERENCES artifacts(digest),
 created_ms INTEGER NOT NULL, expires_ms INTEGER
);
CREATE TABLE approval_revocations(
 revocation_id TEXT PRIMARY KEY, approval_id TEXT NOT NULL REFERENCES approvals(approval_id),
 actor_id TEXT NOT NULL, reason_digest TEXT NOT NULL REFERENCES artifacts(digest), created_ms INTEGER NOT NULL
);
CREATE TABLE budget_accounts(
 run_id TEXT NOT NULL REFERENCES runs(run_id), unit TEXT NOT NULL,
 limit_value INTEGER NOT NULL CHECK(limit_value >= 0), spent_value INTEGER NOT NULL CHECK(spent_value >= 0),
 reserved_value INTEGER NOT NULL CHECK(reserved_value >= 0), PRIMARY KEY(run_id, unit)
);
CREATE TABLE failure_dispositions(
 result_id TEXT PRIMARY KEY REFERENCES check_results(result_id),
 approval_id TEXT NOT NULL REFERENCES approvals(approval_id), created_ms INTEGER NOT NULL
);
CREATE INDEX results_by_run ON check_results(run_id, check_id, configuration_digest);
CREATE INDEX operations_by_state ON operations(state, lease_expires_ms);
CREATE TRIGGER events_no_update BEFORE UPDATE ON events BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;
CREATE TRIGGER events_no_delete BEFORE DELETE ON events BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;
CREATE TRIGGER results_no_update BEFORE UPDATE ON check_results BEGIN SELECT RAISE(ABORT, 'results are immutable'); END;
CREATE TRIGGER results_no_delete BEFORE DELETE ON check_results BEGIN SELECT RAISE(ABORT, 'results are immutable'); END;
CREATE TRIGGER approvals_no_update BEFORE UPDATE ON approvals BEGIN SELECT RAISE(ABORT, 'approvals are immutable'); END;
CREATE TRIGGER approvals_no_delete BEFORE DELETE ON approvals BEGIN SELECT RAISE(ABORT, 'approvals are immutable'); END;
CREATE TRIGGER revocations_no_update BEFORE UPDATE ON approval_revocations BEGIN SELECT RAISE(ABORT, 'revocations are immutable'); END;
CREATE TRIGGER revocations_no_delete BEFORE DELETE ON approval_revocations BEGIN SELECT RAISE(ABORT, 'revocations are immutable'); END;
CREATE TRIGGER receipts_no_update BEFORE UPDATE ON receipts BEGIN SELECT RAISE(ABORT, 'receipts are immutable'); END;
CREATE TRIGGER receipts_no_delete BEFORE DELETE ON receipts BEGIN SELECT RAISE(ABORT, 'receipts are immutable'); END;
CREATE TRIGGER dispositions_no_update BEFORE UPDATE ON failure_dispositions BEGIN SELECT RAISE(ABORT, 'dispositions are immutable'); END;
CREATE TRIGGER dispositions_no_delete BEFORE DELETE ON failure_dispositions BEGIN SELECT RAISE(ABORT, 'dispositions are immutable'); END;
