//! Local materialization of a separate team replica. Its epochs are never
//! passed to the personal store's replication protocol.

use crate::sync::{Change, Page, Stamp};
use crate::{
    Applicability, MemoryKind, MemoryRecord, MemoryStore, Result, SharedMemoryStore, SourceKind,
    VcmError, Verification,
};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

pub(crate) fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS team_imports (
             repo TEXT NOT NULL, link TEXT NOT NULL, replica TEXT NOT NULL,
             epoch TEXT NOT NULL, through INTEGER NOT NULL,
             PRIMARY KEY(repo, link));
         CREATE TABLE IF NOT EXISTS team_bootstrap (repo TEXT NOT NULL,peer TEXT NOT NULL,PRIMARY KEY(repo,peer));
         CREATE TABLE IF NOT EXISTS team_mappings (
             repo TEXT NOT NULL, link TEXT NOT NULL, team_id TEXT NOT NULL,
             local_id TEXT NOT NULL UNIQUE, baseline TEXT NOT NULL,
             PRIMARY KEY(repo, link, team_id));
         CREATE TABLE IF NOT EXISTS team_attribution (repo TEXT NOT NULL,id TEXT NOT NULL,author TEXT NOT NULL,editor TEXT NOT NULL,claim_hash TEXT NOT NULL,PRIMARY KEY(repo,id));
         CREATE TABLE IF NOT EXISTS team_devices (repo TEXT NOT NULL,replica TEXT NOT NULL,principal TEXT NOT NULL,PRIMARY KEY(repo,replica));
         CREATE TABLE IF NOT EXISTS team_consent (repo TEXT NOT NULL,link TEXT NOT NULL,local_id TEXT NOT NULL,PRIMARY KEY(repo,link,local_id));
         CREATE TABLE IF NOT EXISTS team_scan (repo TEXT NOT NULL,link TEXT NOT NULL,through INTEGER NOT NULL,PRIMARY KEY(repo,link));
         CREATE TABLE IF NOT EXISTS team_blocked (repo TEXT NOT NULL,link TEXT NOT NULL,id TEXT NOT NULL,reason TEXT NOT NULL,PRIMARY KEY(repo,link,id));
         CREATE TABLE IF NOT EXISTS team_admin (repo TEXT NOT NULL,operation TEXT NOT NULL,principal TEXT NOT NULL, epoch TEXT NOT NULL,PRIMARY KEY(repo,operation));
         CREATE TABLE IF NOT EXISTS team_claims (repo TEXT NOT NULL,id TEXT NOT NULL,writer TEXT NOT NULL,counter INTEGER NOT NULL,title TEXT NOT NULL,content TEXT NOT NULL,principal TEXT NOT NULL, PRIMARY KEY(repo,id,writer,counter));
         CREATE TABLE IF NOT EXISTS team_purges (repo TEXT NOT NULL,id TEXT NOT NULL,principal TEXT NOT NULL,PRIMARY KEY(repo,id));
         CREATE TABLE IF NOT EXISTS team_history (
             repo TEXT NOT NULL, id TEXT NOT NULL, writer TEXT NOT NULL,
             counter INTEGER NOT NULL, principal TEXT NOT NULL, payload TEXT NOT NULL,
             PRIMARY KEY(repo,id,writer,counter));
         CREATE TABLE IF NOT EXISTS team_outbox (
             repo TEXT NOT NULL, link TEXT NOT NULL, team_id TEXT NOT NULL,
             operation TEXT NOT NULL UNIQUE, payload TEXT, expected TEXT,
             PRIMARY KEY(repo,link,team_id));
         CREATE TABLE IF NOT EXISTS team_recovery (
             repo TEXT NOT NULL,id TEXT NOT NULL,counter INTEGER NOT NULL,recovery_id TEXT NOT NULL,payload TEXT NOT NULL,
             PRIMARY KEY(repo,id,counter));
         CREATE TABLE IF NOT EXISTS team_receipts (
             repo TEXT NOT NULL, operation TEXT NOT NULL, PRIMARY KEY(repo,operation));
         CREATE TABLE IF NOT EXISTS team_withdrawals (
             repo TEXT NOT NULL, link TEXT NOT NULL, local_id TEXT NOT NULL, team_id TEXT NOT NULL,
             PRIMARY KEY(repo,link,local_id));
         CREATE TABLE IF NOT EXISTS team_conflicts (
             repo TEXT NOT NULL, link TEXT NOT NULL, team_id TEXT NOT NULL,
             local_id TEXT NOT NULL PRIMARY KEY);",
    )?;
    Ok(())
}

fn normalize_local_fields(record: &mut MemoryRecord) {
    record.applicability = Applicability::Unknown;
    record.last_retrieved_at = None;
    record.use_count = 0;
    record.success_count = 0;
    record.tags.retain(|tag| !tag.starts_with("chat:"));
}

mod hub;
mod import;
mod publish;
mod receive;

pub use publish::project_record;
