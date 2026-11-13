//! SQLite durability, compare-and-swap transitions and fenced operation leases.
//!
//! This API is a trusted controller adapter, never a worker tool. Every referenced
//! blob is published and verified before its transaction commits.

use crate::{
    artifacts::{private_directory, ArtifactStore},
    id,
    identity::{ArtifactRef, Digest},
    require,
    state::State,
    wire, Error, Result,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path, time::Duration};

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::classified(
            crate::FailureCategory::Internal,
            format!("ledger operation failed: {e}"),
        )
    }
}

fn encoded<T: Serialize>(value: &T) -> Result<String> {
    String::from_utf8(wire::canonical(value)?)
        .map_err(|e| Error::new(format!("invalid metadata encoding: {e}")))
}
fn integer(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| Error::new("ledger integer overflow".into()))
}

fn number(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(e),
        )
    })
}

type StoredRun = (String, u64, String, Option<String>, Option<String>);
type StoredOperation = (String, String, String, u64, String, u64, String, String);

/// Durable state and current revision used for optimistic concurrency.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRecord {
    /// Stable run ID.
    pub run_id: String,
    /// Last committed state.
    pub state: State,
    /// Sequence of the last atomic state event.
    pub revision: u64,
    /// Approved immutable intake.
    pub input_digest: Digest,
    /// Current complete evidence binding, if selected.
    pub binding_digest: Option<Digest>,
    /// Earliest invalidated stage for a blocked run.
    pub restart_state: Option<State>,
}

/// Fencing token returned only after a durable reservation and operation intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    /// Task-owned operation identity.
    pub operation_id: String,
    /// Owning run.
    pub run_id: String,
    /// Monotonically increasing fencing generation.
    pub generation: u64,
    /// Controller instance that claimed this operation.
    pub owner: String,
    /// Deadline, in UTC milliseconds.
    pub expires_ms: u64,
}

/// Per-unit accounting retains actual usage even when it exceeds a reservation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetAccount {
    /// Authorized upper bound.
    pub limit: u64,
    /// Settled actual usage.
    pub spent: u64,
    /// Outstanding operation reservations.
    pub reserved: u64,
}

/// One durable controller store and its immutable blob storage.
pub struct Ledger {
    pub(crate) root: std::path::PathBuf,
    pub(crate) connection: Connection,
    pub(crate) artifacts: ArtifactStore,
}

impl Ledger {
    /// Open controller-private v1 state, initializing the complete schema atomically.
    pub fn open(root: &Path) -> Result<Self> {
        let root = private_directory(root)?;
        let path = root.join("ledger.sqlite3");
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            require(
                meta.is_file() && !meta.file_type().is_symlink(),
                "ledger is not a regular file",
            )?;
        }
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
        )?;
        let version: u32 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        require(version <= 1, "ledger schema is newer than this controller")?;
        if version == 0 {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Recheck after acquiring the write lock: another controller may have migrated.
            let current: u32 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            require(current <= 1, "ledger schema is newer than this controller")?;
            if current == 0 {
                tx.execute_batch(include_str!("../migrations/001_initial.sql"))?;
            }
            tx.commit()?;
        }
        Ok(Self {
            connection,
            artifacts: ArtifactStore::with_source_limit(
                &root.join("artifacts"),
                crate::artifacts::MAX_BUFFER_BYTES,
                crate::artifacts::MAX_SOURCE_BYTES,
            )?,
            root,
        })
    }
    /// Publish and register bytes before creating a ledger reference.
    pub fn store(
        &self,
        kind: &str,
        media_type: &str,
        bytes: &[u8],
        now: u64,
    ) -> Result<ArtifactRef> {
        self.publish(now, |store| store.put(kind, media_type, bytes))
    }
    /// Publish a binary or text source stream with bounded memory.
    pub fn store_source(&self, reader: impl std::io::Read, now: u64) -> Result<ArtifactRef> {
        self.publish(now, |store| store.put_source(reader))
    }
    fn publish(
        &self,
        now: u64,
        publish: impl FnOnce(&ArtifactStore) -> Result<ArtifactRef>,
    ) -> Result<ArtifactRef> {
        // Serialize publication with retention, including reuse of an aged orphan.
        let tx =
            rusqlite::Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        let reference = publish(&self.artifacts)?;
        self.connection.execute(
            "INSERT OR IGNORE INTO artifacts VALUES(?1,?2,?3,?4,?5)",
            params![
                reference.digest.as_str(),
                reference.kind,
                reference.media_type,
                integer(reference.byte_count)?,
                integer(now)?
            ],
        )?;
        let stored = self.reference(&reference.digest)?;
        require(
            stored.byte_count == reference.byte_count,
            "conflicting artifact length",
        )?;
        self.connection.execute(
            "INSERT OR IGNORE INTO artifact_types VALUES(?1,?2,?3)",
            params![
                reference.digest.as_str(),
                reference.kind,
                reference.media_type
            ],
        )?;
        tx.commit()?;
        Ok(reference)
    }
    /// Store canonical JSON, preserving exact bytes used for its digest.
    pub fn store_json(&self, kind: &str, value: &impl Serialize, now: u64) -> Result<ArtifactRef> {
        self.store(kind, "application/json", &wire::canonical(value)?, now)
    }
    /// Resolve a registered identity; arbitrary host paths are not accepted.
    pub fn reference(&self, digest: &Digest) -> Result<ArtifactRef> {
        let (kind, media_type, byte_count) = self.connection.query_row(
            "SELECT kind,media_type,byte_count FROM artifacts WHERE digest=?1",
            [digest.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, number(r, 2)?)),
        )?;
        Ok(ArtifactRef {
            digest: digest.clone(),
            kind,
            media_type,
            byte_count,
        })
    }
    /// Read and integrity-check a registered artifact.
    pub fn read(&self, digest: &Digest) -> Result<Vec<u8>> {
        self.artifacts.read(&self.reference(digest)?)
    }
    /// Stream a registered artifact, checking its identity before the caller publishes it.
    pub fn copy_to(&self, digest: &Digest, writer: impl std::io::Write) -> Result<()> {
        self.artifacts.copy_to(&self.reference(digest)?, writer)
    }
    /// Detect NUL or invalid UTF-8 without allocating the complete source asset.
    pub fn is_binary(&self, digest: &Digest) -> Result<bool> {
        self.artifacts.is_binary(&self.reference(digest)?)
    }
    /// Check both content integrity and the independently registered reference type.
    pub fn verify_reference(&self, reference: &ArtifactRef) -> Result<()> {
        let registered: bool = self.connection.query_row("SELECT EXISTS(SELECT 1 FROM artifact_types WHERE digest=?1 AND kind=?2 AND media_type=?3)", params![reference.digest.as_str(), reference.kind, reference.media_type], |r|r.get(0))?;
        require(registered, "artifact reference type is not registered")?;
        self.artifacts.copy_to(reference, std::io::sink())?;
        Ok(())
    }
    /// Create an intake with explicit budgets. A duplicate run is an error.
    pub fn create_run(
        &mut self,
        run_id: &str,
        input: &Digest,
        budgets: &BTreeMap<String, u64>,
        now: u64,
    ) -> Result<RunRecord> {
        id(run_id)?;
        self.copy_to(input, std::io::sink())?;
        require(!budgets.is_empty(), "run needs explicit budgets")?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO runs VALUES(?1,?2,0,?3,NULL,NULL,?4,?4)",
            params![
                run_id,
                encoded(&State::Intake)?,
                input.as_str(),
                integer(now)?
            ],
        )?;
        for (unit, limit) in budgets {
            id(unit)?;
            tx.execute(
                "INSERT INTO budget_accounts VALUES(?1,?2,?3,0,0)",
                params![run_id, unit, integer(*limit)?],
            )?;
        }
        tx.commit()?;
        self.run(run_id)
    }
    /// Read the durable position, never infer state from leftover build output.
    pub fn run(&self, run_id: &str) -> Result<RunRecord> {
        let (state, revision, input, binding, restart): StoredRun = self.connection.query_row("SELECT state,revision,input_digest,binding_digest,restart_state FROM runs WHERE run_id=?1", [run_id], |r| Ok((r.get(0)?,number(r,1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        Ok(RunRecord {
            run_id: run_id.into(),
            state: wire::decode(state.as_bytes())?,
            revision,
            input_digest: Digest::parse(&input)?,
            binding_digest: binding.map(|v| Digest::parse(&v)).transpose()?,
            restart_state: restart.map(|v| wire::decode(v.as_bytes())).transpose()?,
        })
    }
    /// Commit event and state atomically. Only controller stage handlers call this.
    pub fn transition(
        &mut self,
        expected: &RunRecord,
        next: State,
        event: &Digest,
        restart: Option<State>,
        now: u64,
    ) -> Result<RunRecord> {
        self.copy_to(event, std::io::sink())?;
        expected.state.permits(next, expected.restart_state)?;
        require(
            next != State::Blocked
                || restart.is_some_and(|s| {
                    !s.terminal() && !matches!(s, State::Blocked | State::ReadyForApproval)
                }),
            "blocked run needs a safe restart stage",
        )?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let revision = expected
            .revision
            .checked_add(1)
            .ok_or_else(|| Error::new("run revision exhausted".into()))?;
        let count = tx.execute("UPDATE runs SET state=?1,revision=?2,restart_state=?3,updated_ms=?4 WHERE run_id=?5 AND revision=?6 AND state=?7", params![encoded(&next)?, integer(revision)?, if next == State::Blocked { restart.map(|s| encoded(&s)).transpose()? } else { None }, integer(now)?, expected.run_id, integer(expected.revision)?, encoded(&expected.state)?])?;
        require(count == 1, "concurrent transition: reload the run revision")?;
        tx.execute(
            "INSERT INTO events VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                expected.run_id,
                integer(revision)?,
                encoded(&expected.state)?,
                encoded(&next)?,
                event.as_str(),
                integer(now)?
            ],
        )?;
        tx.commit()?;
        self.run(&expected.run_id)
    }
    /// Select new immutable evidence inputs and record the invalidation event.
    pub fn bind(&mut self, expected: &RunRecord, binding: &Digest, now: u64) -> Result<RunRecord> {
        self.copy_to(binding, std::io::sink())?;
        require(
            !expected.state.terminal() && expected.state != State::Blocked,
            "cannot rebind a blocked or terminal run",
        )?;
        let event = self.store_json(
            "event",
            &serde_json::json!({"kind":"binding_changed", "binding":binding}),
            now,
        )?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let revision = expected
            .revision
            .checked_add(1)
            .ok_or_else(|| Error::new("run revision exhausted".into()))?;
        let count = tx.execute("UPDATE runs SET binding_digest=?1,revision=?2,updated_ms=?3 WHERE run_id=?4 AND revision=?5", params![binding.as_str(), integer(revision)?, integer(now)?, expected.run_id, integer(expected.revision)?])?;
        require(count == 1, "concurrent binding change")?;
        tx.execute(
            "INSERT INTO events VALUES(?1,?2,?3,?3,?4,?5)",
            params![
                expected.run_id,
                integer(revision)?,
                encoded(&expected.state)?,
                event.digest.as_str(),
                integer(now)?
            ],
        )?;
        tx.commit()?;
        self.run(&expected.run_id)
    }
    /// Read all budget units for an operator report.
    pub fn budgets(&self, run_id: &str) -> Result<BTreeMap<String, BudgetAccount>> {
        let mut statement = self.connection.prepare("SELECT unit,limit_value,spent_value,reserved_value FROM budget_accounts WHERE run_id=?1")?;
        let rows = statement.query_map([run_id], |r| {
            Ok((
                r.get(0)?,
                BudgetAccount {
                    limit: number(r, 1)?,
                    spent: number(r, 2)?,
                    reserved: number(r, 3)?,
                },
            ))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
    /// Reserve an operation once; retrying an active key returns the same lease.
    #[allow(clippy::too_many_arguments)]
    pub fn claim(
        &mut self,
        run_id: &str,
        key: &str,
        kind: &str,
        input: &Digest,
        owner: &str,
        now: u64,
        duration_ms: u64,
        reservation: &BTreeMap<String, u64>,
    ) -> Result<Lease> {
        for value in [key, kind, owner] {
            id(value)?;
        }
        require(
            duration_ms > 0 && !reservation.is_empty(),
            "operation requires deadline and reservation",
        )?;
        self.copy_to(input, std::io::sink())?;
        let expires = now
            .checked_add(duration_ms)
            .ok_or_else(|| Error::new("lease expiry overflow".into()))?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state: String =
            tx.query_row("SELECT state FROM runs WHERE run_id=?1", [run_id], |r| {
                r.get(0)
            })?;
        require(
            !matches!(
                wire::decode::<State>(state.as_bytes())?,
                State::Blocked
                    | State::Accepted
                    | State::Cancelled
                    | State::Stopped
                    | State::Rejected
            ),
            "cannot schedule a blocked or terminal run",
        )?;
        let existing: Option<StoredOperation> = tx.query_row("SELECT operation_id,state,input_digest,generation,lease_owner,lease_expires_ms,kind,reservation_json FROM operations WHERE run_id=?1 AND operation_key=?2", params![run_id,key], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,number(r,3)?,r.get(4)?,number(r,5)?,r.get(6)?,r.get(7)?))).optional()?;
        if let Some((
            operation_id,
            state,
            old_input,
            generation,
            lease_owner,
            until,
            old_kind,
            old_reservation,
        )) = existing
        {
            require(
                old_input == input.as_str()
                    && old_kind == kind
                    && old_reservation == encoded(reservation)?,
                "operation key reused with different inputs",
            )?;
            require(
                state == "running" && until > now && lease_owner == owner,
                "operation already finished or lease needs recovery; use a recorded new attempt",
            )?;
            return Ok(Lease {
                operation_id,
                run_id: run_id.into(),
                generation,
                owner: lease_owner,
                expires_ms: until,
            });
        }
        for (unit, amount) in reservation {
            let changed = tx.execute("UPDATE budget_accounts SET reserved_value=reserved_value+?1 WHERE run_id=?2 AND unit=?3 AND spent_value<=limit_value AND reserved_value<=limit_value-spent_value AND ?1<=limit_value-spent_value-reserved_value", params![integer(*amount)?, run_id, unit])?;
            require(changed == 1, format!("budget unavailable: {unit}"))?;
        }
        let operation_id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO operations VALUES(?1,?2,?3,?4,'running',?5,NULL,0,?6,?7,1,?8)",
            params![
                operation_id,
                run_id,
                key,
                kind,
                input.as_str(),
                owner,
                integer(expires)?,
                encoded(reservation)?
            ],
        )?;
        tx.commit()?;
        Ok(Lease {
            operation_id,
            run_id: run_id.into(),
            generation: 0,
            owner: owner.into(),
            expires_ms: expires,
        })
    }
    /// Validate the fencing token immediately before accepting an external callback.
    pub fn verify_lease(&self, lease: &Lease, now: u64) -> Result<()> {
        let found: bool = self.connection.query_row("SELECT EXISTS(SELECT 1 FROM operations o JOIN runs r ON r.run_id=o.run_id WHERE o.operation_id=?1 AND o.run_id=?2 AND generation=?3 AND lease_owner=?4 AND lease_expires_ms=?5 AND lease_expires_ms>?6 AND o.state='running')", params![lease.operation_id,lease.run_id,integer(lease.generation)?,lease.owner,integer(lease.expires_ms)?,integer(now)?], |r|r.get(0))?;
        require(
            found && {
                let state = self.run(&lease.run_id)?.state;
                !state.terminal() && state != State::Blocked
            },
            "stale or unauthenticated operation lease",
        )
    }
    /// Settle actual usage and durable completion; overruns remain visible.
    pub fn complete(
        &mut self,
        lease: &Lease,
        output: &Digest,
        usage: &BTreeMap<String, u64>,
        now: u64,
    ) -> Result<bool> {
        self.copy_to(output, std::io::sink())?;
        self.verify_lease(lease, now)?;
        let usage_receipt = self.store_json("receipt", &serde_json::json!({"operation_id":lease.operation_id,"generation":lease.generation,"actual":usage}), now)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state: String = tx.query_row(
            "SELECT state FROM runs WHERE run_id=?1",
            [&lease.run_id],
            |r| r.get(0),
        )?;
        require(
            !matches!(
                wire::decode::<State>(state.as_bytes())?,
                State::Blocked
                    | State::Accepted
                    | State::Cancelled
                    | State::Stopped
                    | State::Rejected
            ),
            "run paused or terminated before operation completion",
        )?;
        let reservation: String = tx.query_row(
            "SELECT reservation_json FROM operations WHERE operation_id=?1",
            [&lease.operation_id],
            |r| r.get(0),
        )?;
        let reservation: BTreeMap<String, u64> = wire::decode(reservation.as_bytes())?;
        require(
            usage.keys().eq(reservation.keys()),
            "usage receipt must settle every reserved unit",
        )?;
        for (unit, reserved) in &reservation {
            let changed = tx.execute("UPDATE budget_accounts SET reserved_value=reserved_value-?1,spent_value=spent_value+?2 WHERE run_id=?3 AND unit=?4 AND reserved_value>=?1 AND spent_value<=9223372036854775807-?2", params![integer(*reserved)?,integer(usage[unit])?,lease.run_id,unit])?;
            require(
                changed == 1,
                "budget accounting overflow or inconsistent reservation",
            )?;
        }
        let changed = tx.execute("UPDATE operations SET state='completed',output_digest=?1 WHERE operation_id=?2 AND generation=?3 AND lease_owner=?4 AND lease_expires_ms>?5 AND state='running'", params![output.as_str(),lease.operation_id,integer(lease.generation)?,lease.owner,integer(now)?])?;
        require(changed == 1, "operation fenced during completion")?;
        tx.execute(
            "INSERT INTO usage_receipts VALUES(?1,?2,?3)",
            params![
                lease.operation_id,
                usage_receipt.digest.as_str(),
                integer(now)?
            ],
        )?;
        tx.commit()?;
        Ok(self
            .budgets(&lease.run_id)?
            .values()
            .all(|a| a.spent <= a.limit && a.reserved <= a.limit - a.spent))
    }
    /// Abandon expired work without trusting leftover output; keep reservations until reviewed.
    pub fn recover_expired(&mut self, now: u64) -> Result<Vec<String>> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let operations = {
            let mut statement = tx.prepare("SELECT operation_id FROM operations WHERE state='running' AND lease_expires_ms<=?1")?;
            let rows = statement.query_map([integer(now)?], |r| r.get(0))?;
            rows.collect::<std::result::Result<Vec<String>, _>>()?
        };
        tx.execute("UPDATE operations SET state='abandoned',generation=generation+1 WHERE state='running' AND lease_expires_ms<=?1", [integer(now)?])?;
        tx.commit()?;
        Ok(operations)
    }
    /// Validate all registered artifacts on startup or an operator integrity rehearsal.
    pub fn verify_artifacts(&self) -> Result<()> {
        let mut statement = self.connection.prepare("SELECT digest FROM artifacts")?;
        for row in statement.query_map([], |r| r.get::<_, String>(0))? {
            self.copy_to(&Digest::parse(&row?)?, std::io::sink())?;
        }
        Ok(())
    }
}
