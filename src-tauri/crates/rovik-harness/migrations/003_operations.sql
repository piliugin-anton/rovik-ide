CREATE TABLE jobs(
 operation_id TEXT NOT NULL REFERENCES operations(operation_id), generation INTEGER NOT NULL,
 job_id TEXT PRIMARY KEY, workspace TEXT NOT NULL, removed_ms INTEGER,
 UNIQUE(operation_id,generation)
);
CREATE TABLE usage_receipts(
 operation_id TEXT PRIMARY KEY REFERENCES operations(operation_id),
 usage_digest TEXT NOT NULL REFERENCES artifacts(digest), created_ms INTEGER NOT NULL
);
CREATE TABLE reconciliations(
 operation_id TEXT PRIMARY KEY REFERENCES operations(operation_id), actor_id TEXT NOT NULL,
 record_digest TEXT NOT NULL REFERENCES artifacts(digest), created_ms INTEGER NOT NULL
);
CREATE TRIGGER usage_no_update BEFORE UPDATE ON usage_receipts BEGIN SELECT RAISE(ABORT,'usage receipts are immutable'); END;
CREATE TRIGGER usage_no_delete BEFORE DELETE ON usage_receipts BEGIN SELECT RAISE(ABORT,'usage receipts are immutable'); END;
CREATE TRIGGER reconciliation_no_update BEFORE UPDATE ON reconciliations BEGIN SELECT RAISE(ABORT,'reconciliations are immutable'); END;
CREATE TRIGGER reconciliation_no_delete BEFORE DELETE ON reconciliations BEGIN SELECT RAISE(ABORT,'reconciliations are immutable'); END;
CREATE TABLE failure_resolutions(
 resolution_id TEXT PRIMARY KEY, result_id TEXT NOT NULL REFERENCES check_results(result_id),
 approval_id TEXT NOT NULL REFERENCES approvals(approval_id), created_ms INTEGER NOT NULL,
 UNIQUE(result_id,approval_id)
);
INSERT INTO failure_resolutions SELECT result_id || ':' || approval_id,result_id,approval_id,created_ms FROM failure_dispositions;
CREATE TRIGGER resolution_no_update BEFORE UPDATE ON failure_resolutions BEGIN SELECT RAISE(ABORT,'resolutions are immutable'); END;
CREATE TRIGGER resolution_no_delete BEFORE DELETE ON failure_resolutions BEGIN SELECT RAISE(ABORT,'resolutions are immutable'); END;
INSERT INTO schema_versions VALUES(3,0);
PRAGMA user_version=3;
